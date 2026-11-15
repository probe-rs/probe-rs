use super::{
    DebugError, DebugRegisters, StackFrame, VariableCache,
    function_die::{Die, FunctionDie},
    get_object_reference,
    unit_info::UnitInfo,
    variable::*,
};
use crate::{SourceLocation, VerifiedBreakpoint, stack_frame::StackFrameInfo, unit_info::RangeExt};
use gimli::{DebugInfoOffset, RunTimeEndian};
use object::read::{Object, ObjectSection};
use probe_rs::{Endian, MemoryInterface, RegisterValue};
use std::{
    borrow,
    cmp::Ordering,
    num::NonZeroU64,
    path::Path,
    str::from_utf8,
    sync::{Arc, Mutex},
};
use typed_path::{TypedPath, TypedPathBuf};

mod unwind;

use unwind::FdeIndex;
pub use unwind::{get_unwind_info, unwind_pc_without_debuginfo, unwind_program_counter_register};

pub(crate) type GimliReader = gimli::EndianReader<RunTimeEndian, std::sync::Arc<[u8]>>;
pub(crate) type GimliReaderOffset =
    <gimli::EndianReader<RunTimeEndian, Arc<[u8]>> as gimli::Reader>::Offset;

pub(crate) type GimliAttribute = gimli::Attribute<GimliReader>;

pub(crate) type DwarfReader = gimli::read::EndianArcSlice<RunTimeEndian>;

/// Debug information which is parsed from DWARF debugging information.
pub struct DebugInfo {
    pub(crate) dwarf: gimli::Dwarf<DwarfReader>,
    pub(crate) frame_section: gimli::DebugFrame<DwarfReader>,
    pub(crate) fde_index: FdeIndex,
    pub(crate) debug_line_section: gimli::DebugLine<DwarfReader>,

    pub(crate) unit_infos: Vec<UnitInfo>,
    pub(crate) endianness: gimli::RunTimeEndian,

    /// Cached symbol-table fallback for frames without DWARF.
    ///
    /// Wrapped in a [`Mutex`] because `addr2line::Loader` is `Send` but not
    /// `Sync`, while [`DebugInfo`] must be both so an RPC server can share it.
    pub(crate) addr2line: Option<Mutex<Box<addr2line::Loader>>>,
}

impl DebugInfo {
    /// Read debug info directly from an ELF file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<DebugInfo, DebugError> {
        let data = std::fs::read(path.as_ref())?;

        let mut this = DebugInfo::from_raw(&data)?;
        this.addr2line = addr2line::Loader::new(path)
            .ok()
            .map(Box::new)
            .map(Mutex::new);
        Ok(this)
    }

    /// Parse debug information directly from a buffer containing an ELF file.
    pub fn from_raw(data: &[u8]) -> Result<Self, DebugError> {
        let object = object::File::parse(data)?;

        let endianness = if object.is_little_endian() {
            RunTimeEndian::Little
        } else {
            RunTimeEndian::Big
        };

        // Load a section and return as `Cow<[u8]>`.
        let load_section = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            let data = object
                .section_by_name(id.name())
                .and_then(|section| section.uncompressed_data().ok())
                .unwrap_or_else(|| borrow::Cow::Borrowed(&[][..]));

            Ok(gimli::read::EndianArcSlice::new(
                Arc::from(&*data),
                endianness,
            ))
        };

        // Load all of the sections.
        let dwarf_cow = gimli::Dwarf::load(&load_section)?;

        use gimli::Section;
        let mut frame_section = gimli::DebugFrame::load(load_section)?;
        let debug_line_section = gimli::DebugLine::load(load_section)?;

        let mut unit_infos = Vec::new();

        let mut iter = dwarf_cow.units();

        while let Ok(Some(header)) = iter.next() {
            if let Ok(unit) = dwarf_cow.unit(header) {
                // The DWARF V5 standard, section 2.4 specifies that the address size
                // for the object file (or the target architecture default) will be used for
                // DWARF debugging information.
                // The following line is a workaround for instances where the address size of the
                // CIE (Common Information Entry) is not correctly set.
                // The frame section address size is only used for CIE versions before 4.
                frame_section.set_address_size(unit.encoding().address_size);

                unit_infos.push(UnitInfo::new(unit, &dwarf_cow));
            };
        }

        Ok(DebugInfo {
            dwarf: dwarf_cow,
            fde_index: FdeIndex::new(&frame_section),
            frame_section,
            debug_line_section,
            unit_infos,
            endianness,
            addr2line: None,
        })
    }

    pub(crate) fn call_site_value(
        &self,
        return_pc: u64,
        location: &gimli::Expression<GimliReader>,
    ) -> Option<(&UnitInfo, gimli::Expression<GimliReader>)> {
        self.unit_infos.iter().find_map(|unit| {
            unit.call_site_value(return_pc, location)
                .map(|expression| (unit, expression))
        })
    }

    /// Create a [`DebugInfo`] that contains no debug information, for a target of the given
    /// endianness.
    ///
    /// Unwinding with this still yields a backtrace: when the unwinder finds no unwind info for a
    /// frame it falls back to the architecture's calling convention (for example the Xtensa window
    /// save area). The resulting frames carry a program counter but no name or source location.
    pub fn empty(endian: Endian) -> Self {
        let endianness = match endian {
            Endian::Little => RunTimeEndian::Little,
            Endian::Big => RunTimeEndian::Big,
        };
        let load_section = |_id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            Ok(gimli::read::EndianArcSlice::new(
                Arc::from(&[][..]),
                endianness,
            ))
        };

        use gimli::Section;
        let load = || -> Result<Self, gimli::Error> {
            Ok(DebugInfo {
                dwarf: gimli::Dwarf::load(&load_section)?,
                frame_section: gimli::DebugFrame::load(load_section)?,
                fde_index: FdeIndex::default(),
                debug_line_section: gimli::DebugLine::load(load_section)?,
                unit_infos: Vec::new(),
                endianness,
                addr2line: None,
            })
        };

        load().expect("loading empty DWARF sections cannot fail")
    }

    /// Try get the [`SourceLocation`] for a given address.
    pub fn get_source_location(&self, address: u64) -> Option<SourceLocation> {
        for unit_info in &self.unit_infos {
            let unit = &unit_info.unit;

            let mut ranges = match self.dwarf.unit_ranges(unit) {
                Ok(ranges) => ranges,
                Err(error) => {
                    tracing::warn!(
                        "No valid source code ranges found for unit {:?}: {:?}",
                        unit.dwo_name(),
                        error
                    );
                    continue;
                }
            };

            if !ranges.contains(address) {
                continue;
            }

            // Get the DWARF LineProgram.
            let line_program = unit.line_program.clone()?;
            if let Some(location) = self.line_program_source_location(unit, line_program, address) {
                return Some(location);
            }
        }
        None
    }

    /// Look up the [`SourceLocation`] of `address` in a line program of `unit`.
    fn line_program_source_location(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        line_program: gimli::IncompleteLineProgram<DwarfReader>,
        address: u64,
    ) -> Option<SourceLocation> {
        let (program, sequences) = match line_program.sequences() {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(
                    "No valid source code ranges found for address {}: {:?}",
                    address,
                    error
                );
                return None;
            }
        };

        let target_seq = sequences
            .iter()
            .find(|seq| seq.start <= address && address < seq.end)?;

        let mut previous_row: Option<gimli::LineRow> = None;

        let mut rows = program.resume_from(target_seq);

        while let Ok(Some((_, row))) = rows.next_row() {
            let matching_row = match row.address().cmp(&address) {
                // The address is after the current row, so we use the previous row data.
                //
                // (If we don't do this, you get the artificial effect where the debugger
                // steps to the top of the file when it is stepping out of a function.)
                Ordering::Greater => previous_row,
                Ordering::Less => None,
                Ordering::Equal => Some(*row),
            };
            if let Some(location) =
                matching_row.and_then(|row| self.row_source_location(unit, &row, address))
            {
                return Some(location);
            }
            previous_row = Some(*row);
        }
        None
    }

    fn row_source_location(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        row: &gimli::LineRow,
        address: u64,
    ) -> Option<SourceLocation> {
        let path = self.get_path(unit, row.file_index())?;
        tracing::debug!("{:#010x} - {:?}", address, row.isa());

        Some(SourceLocation {
            line: row.line().map(NonZeroU64::get),
            column: Some(row.column().into()),
            path,
            address: Some(row.address()),
        })
    }

    /// We do not actually resolve the children of `[VariableName::StaticScope]` automatically,
    /// and only create the necessary header in the `VariableCache`.
    /// This allows us to resolve the `[VariableName::StaticScope]` on demand/lazily, when a user requests it from the debug client.
    /// This saves a lot of overhead when a user only wants to see the `[VariableName::LocalScope]` or `
    /// [VariableName::Registers]` while stepping through code (the most common use cases)
    pub fn create_static_scope_cache(&self) -> VariableCache {
        VariableCache::new_static_cache()
    }

    /// Creates the unpopulated cache for `function` variables
    pub(crate) fn create_function_scope_cache(
        &self,
        die_cursor_state: &FunctionDie,
        unit_info: &UnitInfo,
    ) -> Result<VariableCache, DebugError> {
        let function_variable_cache = VariableCache::new_dwarf_cache(
            die_cursor_state.function_die.offset(),
            VariableName::LocalScopeRoot,
            unit_info,
        )?;

        Ok(function_variable_cache)
    }

    fn unit_info_at(&self, header_offset: DebugInfoOffset) -> Result<&UnitInfo, DebugError> {
        self.unit_infos
            .iter()
            .find(|unit_info| {
                unit_info
                    .unit
                    .header
                    .offset()
                    .to_debug_info_offset(&unit_info.unit)
                    == Some(header_offset)
            })
            .ok_or_else(|| {
                DebugError::Other("Failed to find unit info for offset lookup.".to_string())
            })
    }

    /// This effects the on-demand expansion of lazy/deferred load of all the 'child' `Variable`s for a given 'parent'.
    #[tracing::instrument(level = "trace", skip_all, fields(parent_variable = ?parent_variable.variable_key()))]
    pub fn cache_deferred_variables(
        &self,
        cache: &mut VariableCache,
        memory: &mut dyn MemoryInterface,
        parent_variable: &mut Variable,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        if !parent_variable.is_valid() {
            // Do nothing. The parent_variable.get_value() will already report back the debug_error value.
            return Ok(());
        }

        // Only attempt this part if we have not yet resolved the referenced children.
        if cache.has_children(parent_variable) {
            return Ok(());
        }

        match parent_variable.variable_node_type {
            VariableNodeType::PointerTarget(header_offset, type_offset) => {
                let unit_info = self.unit_info_at(header_offset)?;
                unit_info.resolve_pointer_target(
                    self,
                    type_offset,
                    parent_variable,
                    memory,
                    cache,
                    frame_info,
                )?;
            }
            VariableNodeType::TypeOffset(header_offset, unit_offset)
            | VariableNodeType::DirectLookup(header_offset, unit_offset) => {
                let unit_info = self.unit_info_at(header_offset)?;

                // Find the parent node
                let mut type_tree = unit_info.unit.entries_tree(Some(unit_offset))?;
                let parent_node = type_tree.root()?;

                unit_info.process_tree(
                    self,
                    parent_node,
                    parent_variable,
                    memory,
                    cache,
                    frame_info,
                )?;

                // Members are in the cache. `process_struct` can rewrite them now.
                let mut type_tree = unit_info.unit.entries_tree(Some(unit_offset))?;
                unit_info.process_struct(
                    self,
                    type_tree.root()?.entry(),
                    parent_variable,
                    memory,
                    cache,
                    frame_info,
                )?;
            }
            VariableNodeType::UnitsLookup => {
                if self.unit_infos.is_empty() {
                    // No unit infos
                    return Err(DebugError::Other("Missing unit infos".to_string()));
                }

                // Look up static variables from all units
                for unit_info in self.unit_infos.iter() {
                    let mut entries = unit_info.unit.entries();

                    // Only process statics for this unit header.
                    // Navigate the current unit from the header down.
                    let unit_node = entries.next_dfs()?.unwrap();
                    let unit_offset = unit_node.offset();

                    unit_info.collect_static_variables(
                        self,
                        unit_offset,
                        parent_variable.variable_key(),
                        memory,
                        cache,
                        frame_info,
                    )?;
                }
            }
            _ => {
                // Do nothing. These have already been recursed to their maximum.
            }
        }
        Ok(())
    }

    /// The name of the symbol that covers `address`, if a symbol table is available.
    pub(crate) fn find_symbol(&self, address: u64) -> Option<String> {
        // `Loader` is not `Sync`; serialize lookups against the shared cache.
        let addr2line = self.addr2line.as_ref()?.lock().ok()?;
        addr2line.find_symbol(address).map(str::to_string)
    }

    /// Best-effort way to look up a function name without debuginfo.
    fn get_stackframe_from_symbols(
        &self,
        address: u64,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let Some(mut fn_name) = self.find_symbol(address) else {
            return Ok(vec![]);
        };

        for lang in [
            gimli::DW_LANG_Rust,
            gimli::DW_LANG_C_plus_plus,
            gimli::DW_LANG_C_plus_plus_03,
            gimli::DW_LANG_C_plus_plus_11,
            gimli::DW_LANG_C_plus_plus_14,
        ] {
            if let Some(demangle) = addr2line::demangle(&fn_name, lang) {
                fn_name = crate::language::from_dwarf(lang).compact_debug_name(&demangle);
                break;
            }
        }

        Ok(vec![StackFrame {
            id: get_object_reference(),
            function_name: fn_name,
            source_location: None,
            registers: unwind_registers.clone(),
            pc: unwind_registers.address_to_register_value(address),
            frame_base: None,
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: None,
        }])
    }

    /// Returns a populated (resolved) [`StackFrame`] struct.
    /// This function will also populate the `DebugInfo::VariableCache` with in scope `Variable`s for each `StackFrame`,
    /// while taking into account the appropriate strategy for lazy-loading of variables.
    pub fn get_stackframe_info(
        &self,
        memory: &mut impl MemoryInterface,
        address: u64,
        cfa: Option<u64>,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let unknown_function = || unknown_function_name(address, unwind_registers);

        let Ok((unit_info, functions)) = self.get_function_dies(address) else {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        };
        let (Some(first_function), Some(last_function)) = (functions.first(), functions.last())
        else {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        };

        // The first function is the non-inlined function, and the rest are inlined functions.
        // The frame base only exists for the non-inlined function, so we can reuse it for all the inlined functions.
        let frame_base = first_function.frame_base(
            self,
            memory,
            &StackFrameInfo {
                registers: unwind_registers,
                frame_base: None,
                canonical_frame_address: cfa,
                caller: None,
            },
        )?;

        let stack_frame = |function_die: &FunctionDie<'_>,
                           function_name: String,
                           source_location: Option<SourceLocation>,
                           pc: RegisterValue| {
            // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
            // Resolve and cache the function variables.
            let local_variables = self
                .create_function_scope_cache(function_die, unit_info)
                .inspect_err(|error| {
                    tracing::error!("Could not resolve function variables. {error}. Continuing...");
                })
                .ok();

            StackFrame {
                id: get_object_reference(),
                function_name,
                source_location,
                registers: unwind_registers.clone(),
                pc,
                frame_base,
                is_inlined: function_die.is_inline(),
                local_variables,
                canonical_frame_address: cfa,
            }
        };

        let mut frames = Vec::new();

        // Handle all functions which contain further inlined functions. For
        // these functions, the location is the call site of the inlined function.
        for (function_die, next_function) in functions.iter().zip(&functions[1..]) {
            let function_name = function_die
                .function_name(self)
                .unwrap_or_else(unknown_function);

            tracing::debug!("UNWIND: Function name: {}", function_name);

            assert!(next_function.is_inline());

            let Some(call_site) = inlined_call_site(next_function, unit_info, &function_name)
            else {
                continue;
            };
            let inlined_call_site = unwind_registers.address_to_register_value(call_site);

            tracing::debug!(
                "UNWIND: Callsite for inlined function {:?}",
                next_function.function_name(self)
            );

            let inlined_caller_source_location = next_function.inline_call_location(self);

            tracing::debug!("UNWIND: Call site: {inlined_caller_source_location:?}");

            frames.push(stack_frame(
                function_die,
                function_name,
                inlined_caller_source_location,
                inlined_call_site,
            ));
        }

        // Handle last function, which contains no further inlined functions
        let function_name = last_function
            .function_name(self)
            .unwrap_or_else(unknown_function);

        let function_location = self.get_source_location(address);

        frames.push(stack_frame(
            last_function,
            function_name,
            function_location,
            unwind_registers.address_to_register_value(address),
        ));

        Ok(frames)
    }

    /// Find the program counter where a breakpoint should be set,
    /// given a source file, a line and optionally a column.
    // TODO: Move (and fix) this to the [`InstructionSequence::for_source_location`] method.
    #[tracing::instrument(skip_all)]
    pub fn get_breakpoint_location(
        &self,
        path: TypedPath,
        line: u64,
        column: Option<u64>,
    ) -> Result<VerifiedBreakpoint, DebugError> {
        tracing::debug!(
            "Looking for breakpoint location for {}:{}:{}",
            path.display(),
            line,
            column
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_owned())
        );
        VerifiedBreakpoint::for_source_location(self, path, line, column)
    }

    /// Get the path for an entry in a line program header, using the compilation unit's directory and file entries.
    // TODO: Determine if it is necessary to navigate the include directories to find the file absolute path for C files.
    pub(crate) fn get_path(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        file_index: u64,
    ) -> Option<TypedPathBuf> {
        let line_program = unit.line_program.as_ref()?;
        let header = line_program.header();
        let Some(file_entry) = header.file(file_index) else {
            tracing::warn!(
                "Unable to extract file entry for file_index {:?}.",
                file_index
            );
            return None;
        };
        let file_name_attr_string = self.dwarf.attr_string(unit, file_entry.path_name()).ok()?;
        let name_path = from_utf8(&file_name_attr_string).ok()?;

        let dir_name_attr_string = file_entry
            .directory(header)
            .and_then(|dir| self.dwarf.attr_string(unit, dir).ok());

        let dir_path = dir_name_attr_string.and_then(|dir_name| {
            from_utf8(&dir_name)
                .ok()
                .map(|p| TypedPath::derive(p).to_path_buf())
        });

        let mut combined_path = match dir_path {
            Some(dir_path) => dir_path.join(name_path),
            None => TypedPath::derive(name_path).to_path_buf(),
        };

        if combined_path.is_relative() {
            let comp_dir = unit
                .comp_dir
                .as_ref()
                .map(|dir| from_utf8(dir))
                .transpose()
                .ok()?
                .map(TypedPath::derive);
            if let Some(comp_dir) = comp_dir {
                combined_path = comp_dir.join(&combined_path);
            }
        }

        Some(combined_path)
    }

    // Return the compilation unit that contains the given address
    pub(crate) fn compile_unit_info(&self, address: u64) -> Result<&UnitInfo, DebugError> {
        self.unit_infos
            .iter()
            .find(|unit_info| {
                self.dwarf
                    .unit_ranges(&unit_info.unit)
                    .is_ok_and(|mut ranges| ranges.contains(address))
            })
            .ok_or_else(|| DebugError::WarnAndContinue {
                message: format!(
                    "No debug information available for the instruction at {address:#010x}. Please consider using instruction level stepping."
                ),
            })
    }

    /// Search across all compilation units, and retrieve the DIEs for the function containing the given address.
    /// This is distinct from [`UnitInfo::get_function_dies`] in that it will search all compilation units.
    /// - The first entry in the vector will be the outermost function containing the address.
    /// - If the address is inlined, the innermost function will be the last entry in the vector.
    pub(crate) fn get_function_dies(
        &self,
        address: u64,
    ) -> Result<(&UnitInfo, Vec<FunctionDie<'_>>), DebugError> {
        for unit_info in &self.unit_infos {
            let function_dies = unit_info.get_function_dies(self, address)?;

            if !function_dies.is_empty() {
                return Ok((unit_info, function_dies));
            }
        }
        Err(DebugError::Other(format!(
            "No function DIE's at address {address:#x}."
        )))
    }

    /// Look up the DIE reference for the given attribute, returning both the resolved DIE
    /// and the compilation unit it belongs to.
    ///
    /// The resolved unit can differ from `unit_info` when the reference crosses a compilation
    /// unit boundary (`DW_FORM_ref_addr`). Callers that subsequently follow *unit-relative*
    /// references (e.g. `DW_AT_specification`) from the resolved DIE must use the returned unit,
    /// not the unit the reference originated from.
    pub(crate) fn resolve_die_reference_with_unit_info<'debug_info, 'unit_info>(
        &'debug_info self,
        attribute: gimli::DwAt,
        die: &Die,
        unit_info: &'unit_info UnitInfo,
    ) -> Option<(&'debug_info UnitInfo, Die)>
    where
        'unit_info: 'debug_info,
    {
        let attr = die.attr(attribute)?;

        self.resolve_die_reference_with_unit(attr, unit_info).ok()
    }

    /// The program binary's (and core's) endianness.
    pub fn endianness(&self) -> RunTimeEndian {
        self.endianness
    }

    /// Returns the UnitInfo and DIE for the given attribute.
    pub(crate) fn resolve_die_reference_with_unit<'debug_info, 'unit_info>(
        &'debug_info self,
        attr: &gimli::Attribute<GimliReader>,
        unit_info: &'unit_info UnitInfo,
    ) -> Result<(&'debug_info UnitInfo, Die), DebugError>
    where
        'unit_info: 'debug_info,
    {
        match attr.value() {
            gimli::AttributeValue::UnitRef(unit_ref) => {
                Ok((unit_info, unit_info.unit.entry(unit_ref)?))
            }
            gimli::AttributeValue::DebugInfoRef(offset) => self.entry_at_debug_info_offset(offset),
            other_attribute_value => Err(DebugError::Other(format!(
                "Unimplemented attribute value {other_attribute_value:?}"
            ))),
        }
    }

    /// Returns the UnitInfo and DIE at the given offset into the debug info.
    pub(crate) fn entry_at_debug_info_offset(
        &self,
        offset: gimli::DebugInfoOffset,
    ) -> Result<(&UnitInfo, Die), DebugError> {
        for unit_info in &self.unit_infos {
            let Some(unit_offset) = offset.to_unit_offset(&unit_info.unit.header) else {
                continue;
            };

            let entry = unit_info.unit.entry(unit_offset).map_err(|error| {
                DebugError::Other(format!(
                    "Error reading DIE at debug info offset {:#x} : {}",
                    offset.0, error
                ))
            })?;
            return Ok((unit_info, entry));
        }

        Err(DebugError::Other(format!(
            "Unable to find unit info for debug info offset {:#x}",
            offset.0
        )))
    }
}

/// The name of a frame at `address` whose function is unknown.
///
/// The address is a hex string, with the width matching the configured size of the datatype
/// used in the `RegisterValue` address.
fn unknown_function_name(address: u64, registers: &DebugRegisters) -> String {
    format!(
        "<unknown function @ {address:#0width$x}>",
        width = (registers.get_address_size_bytes() * 2 + 2)
    )
}

/// The call site of the inlined `function`, which is its first instruction. `caller_name` names
/// the function that `function` is inlined into.
fn inlined_call_site(
    function: &FunctionDie,
    unit_info: &UnitInfo,
    caller_name: &str,
) -> Option<u64> {
    let address_size = unit_info.unit.header.address_size() as u64;

    let Some(low_pc) = function.low_pc() else {
        tracing::warn!("UNWIND: Unknown starting address for inlined function {caller_name}.");
        return None;
    };

    if !(low_pc > address_size && low_pc < u32::MAX as u64) {
        tracing::warn!("UNWIND: Unknown call site for inlined function {caller_name}.");
        return None;
    }

    Some(low_pc)
}

/// Returns `true` if `full_path` matches `partial_path`.
///
/// When `partial_path` is absolute, the comparison is normalized equality.
/// When `partial_path` is relative the function additionally accepts a suffix match at
/// a component boundary, so that a caller can supply just a filename (`main.rs`) or a partial
/// sub-path (`src/main.rs`) and still resolve to the correct compilation unit.
///
/// If one of the paths is a Windows path, both are normalized to lowercase before comparison.
///
/// Separators are normalized to `/` before comparison so that cross-OS paths (e.g. DWARF info
/// embedded by a Windows compiler, inspected on Linux) are handled correctly.
pub(crate) fn path_matches(full_path: TypedPath, partial_path: TypedPath) -> bool {
    let ignore_case = full_path.is_windows() || partial_path.is_windows();
    let normalize_path = |path: TypedPath| {
        let string = path.normalize().to_string_lossy().replace('\\', "/");

        if ignore_case {
            string.to_lowercase()
        } else {
            string
        }
    };

    let full_str = normalize_path(full_path);
    let partial_str = normalize_path(partial_path);

    match full_str.strip_suffix(&partial_str) {
        Some("") => true,
        Some(prefix) => partial_path.is_relative() && prefix.ends_with('/'),
        None => false,
    }
}

#[cfg(test)]
mod test {
    use crate::{
        DebugInfo, DebugRegister, DebugRegisters, ObjectRef, VariableCache,
        exception_handling::{
            armv6m::ArmV6MExceptionHandler, armv7m::ArmV7MExceptionHandler,
            exception_handler_for_core,
        },
        stack_frame::{StackFrameInfo, TestFormatter},
    };

    use probe_rs::{
        CoreDump, MemoryInterface, RegisterValue,
        architecture::arm::core::registers::cortex_m::CORTEX_M_CORE_REGISTERS, test::MockMemory,
    };
    use std::path::{Path, PathBuf};
    use test_case::test_case;

    use super::{TypedPath, path_matches};

    /// Get the full path to a file in the `tests` directory.
    fn get_path_for_test_files(relative_file: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("tests");
        path.push(relative_file);
        path
    }

    /// Load the DebugInfo from the `elf_file` for the test.
    /// `elf_file` should be the name of a file(or relative path) in the `tests` directory.
    fn load_test_elf_as_debug_info(elf_file: &str) -> DebugInfo {
        let path = get_path_for_test_files(elf_file);
        DebugInfo::from_file(&path).unwrap_or_else(|err: crate::DebugError| {
            panic!("Failed to open file {}: {:?}", path.display(), err)
        })
    }

    #[test]
    fn unwinding_first_instruction_after_exception() {
        let debug_info = load_test_elf_as_debug_info("exceptions");

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001fff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffd0
        // R14       : 0xfffffff9
        // R15       : 0x00000182
        // MSP       : 0x2001ffd0
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000
        // FPSCR     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ffcf, // R1
            0x20000044, // R2
            0x20000044, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001fff0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffd0, // R13
            0xfffffff9, // R14
            0x00000182, // R15
            0x2001ffd0, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let expected_regs = regs.clone();

        let mut mocked_mem = MockMemory::new();

        // Stack:
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        mocked_mem.add_word_range(
            0x2001_ffd0,
            &[
                0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000, 0x0000017f, 0x00000180,
                0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind(
                &mut mocked_mem,
                regs,
                exception_handler.as_ref(),
                Some(probe_rs_target::InstructionSet::Thumb2),
                500,
            )
            .unwrap();

        let first_frame = &frames[0];

        assert_eq!(first_frame.pc, RegisterValue::U32(0x00000182));

        assert_eq!(
            first_frame.function_name,
            "__cortex_m_rt_SVCall_trampoline".to_string()
        );

        assert_eq!(first_frame.registers, expected_regs);

        let next_frame = &frames[1];
        assert_eq!(next_frame.function_name, "SVC");
        assert_eq!(next_frame.pc, RegisterValue::U32(0x0000017f));

        // Expected stack frame(s):
        // Frame 0: __cortex_m_rt_SVCall_trampoline @ 0x00000182
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:22:1
        //
        // <--- A frame seems to be missing here, to indicate the exception entry
        //
        // Frame 1: __cortex_m_rt_main @ 0x00000180   (<--- This should be 0x17e). See the doc comment
        // on probe_rs::architecture::arm::core::exception_handling::armv6m_armv7m_shared::EXCEPTION_STACK_REGISTERS
        // for the explanation of why this is the case.
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:19:5
        // Frame 2: __cortex_m_rt_main_trampoline @ 0x00000160
        //        /home/dominik/code/probe-rs/probe-rs-repro/nrf/exceptions/src/main.rs:11:1
        // Frame 3: memmove @ 0x0000013c
        // Frame 4: memmove @ 0x0000013c

        // Registers in frame 1:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001fff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001fff0
        // R14       : 0x0000017f
        // R15       : 0x0000017e
        // MSP       : 0x2001fff0
        // PSP       : 0x00000000
        // XPSR      : 0x21000000
        // EXTRA     : 0x00000000
        // XPSR      : 0x21000000
    }

    #[test]
    fn unwinding_in_exception_handler() {
        let debug_info = load_test_elf_as_debug_info("exceptions");

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ff9f
        // R2        : 0x20000047
        // R3        : 0x20000047
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001ffc0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffc0
        // R14       : 0x0000042f
        // R15       : 0x000001a4
        // MSP       : 0x2001ffc0
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ff9f, // R1
            0x20000047, // R2
            0x20000047, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001ffc0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffc0, // R13
            0x0000042f, // R14
            0x000001a4, // R15
            0x2001ffc0, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x2001ffc0 = 0x2001ffc8
        // 0x2001ffc4 = 0x0000018b
        // 0x2001ffc8 = 0x2001fff0
        // 0x2001ffcc = 0xfffffff9
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2001_ffc0,
            &[
                0x2001ffc8, 0x0000018b, 0x2001fff0, 0xfffffff9, 0x00000001, 0x2001ffcf, 0x20000044,
                0x20000044, 0x00000000, 0x0000017f, 0x00000180, 0x21000000, 0x2001fff8, 0x00000161,
                0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind(
                &mut dummy_mem,
                regs,
                exception_handler.as_ref(),
                Some(probe_rs_target::InstructionSet::Thumb2),
                500,
            )
            .unwrap();

        assert_eq!(frames[0].pc, RegisterValue::U32(0x000001a4));

        assert_eq!(
            frames[1].function_name,
            "__cortex_m_rt_SVCall_trampoline".to_string()
        );

        assert_eq!(frames[1].pc, RegisterValue::U32(0x00000188)); // <-- This is the instruction for the jump into the topmost frame.

        // The PC value in the exception data
        // depends on the exception type, and for some exceptions, it will
        // be the address of the instruction that caused the exception, while for other exceptions
        // it will be the address of the next instruction after the instruction that caused the exception.
        // See: https://developer.arm.com/documentation/ddi0403/d/System-Level-Architecture/System-Level-Programmers--Model/ARMv7-M-exception-model/Exception-entry-behavior?lang=en
        assert_eq!(
            frames[1]
                .registers
                .get_register(probe_rs::RegisterId(7))
                .and_then(|r| r.value),
            Some(RegisterValue::U32(0x2001ffc8))
        );

        let printed_backtrace = frames
            .into_iter()
            .map(|f| TestFormatter(&f).to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }

    #[test]
    fn unwinding_in_exception_trampoline() {
        let debug_info = load_test_elf_as_debug_info("exceptions");

        // Registers:
        // R0        : 0x00000001
        // R1        : 0x2001ffcf
        // R2        : 0x20000044
        // R3        : 0x20000044
        // R4        : 0x00000000
        // R5        : 0x00000000
        // R6        : 0x00000000
        // R7        : 0x2001ffc8
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x00000000
        // R13       : 0x2001ffc8
        // R14       : 0x0000018B
        // R15       : 0x0000018A
        // MSP       : 0x2001ffc8
        // PSP       : 0x00000000
        // XPSR      : 0x2100000b
        // EXTRA     : 0x00000000

        let values: Vec<_> = [
            0x00000001, // R0
            0x2001ffcf, // R1
            0x20000044, // R2
            0x20000044, // R3
            0x00000000, // R4
            0x00000000, // R5
            0x00000000, // R6
            0x2001ffc8, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x00000000, // R12
            0x2001ffc8, // R13
            0x0000018B, // R14
            0x0000018A, // R15
            0x2001ffc8, // MSP
            0x00000000, // PSP
            0x2100000b, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x2001ffc8 = 0x2001fff0
        // 0x2001ffcc = 0xfffffff9
        // 0x2001ffd0 = 0x00000001
        // 0x2001ffd4 = 0x2001ffcf
        // 0x2001ffd8 = 0x20000044
        // 0x2001ffdc = 0x20000044
        // 0x2001ffe0 = 0x00000000
        // 0x2001ffe4 = 0x0000017f
        // 0x2001ffe8 = 0x00000180
        // 0x2001ffec = 0x21000000
        // 0x2001fff0 = 0x2001fff8
        // 0x2001fff4 = 0x00000161
        // 0x2001fff8 = 0x00000000
        // 0x2001fffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2001_ffc8,
            &[
                0x2001fff0, 0xfffffff9, 0x00000001, 0x2001ffcf, 0x20000044, 0x20000044, 0x00000000,
                0x0000017f, 0x00000180, 0x21000000, 0x2001fff8, 0x00000161, 0x00000000, 0x0000013d,
            ],
        );

        let exception_handler = Box::new(ArmV6MExceptionHandler {});

        let frames = debug_info
            .unwind(
                &mut dummy_mem,
                regs,
                exception_handler.as_ref(),
                Some(probe_rs_target::InstructionSet::Thumb2),
                500,
            )
            .unwrap();

        let printed_backtrace = frames
            .into_iter()
            .map(|f| TestFormatter(&f).to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }

    #[test]
    fn unwinding_inlined() {
        let debug_info = load_test_elf_as_debug_info("inlined-functions");

        // Registers:
        // R0        : 0xfffffecc
        // R1        : 0x00000001
        // R2        : 0x00000000
        // R3        : 0x40008140
        // R4        : 0x000f4240
        // R5        : 0xfffffec0
        // R6        : 0x00000000
        // R7        : 0x20003ff0
        // R8        : 0x00000000
        // R9        : 0x00000000
        // R10       : 0x00000000
        // R11       : 0x00000000
        // R12       : 0x5000050c
        // R13       : 0x20003ff0
        // R14       : 0x00200000
        // R15       : 0x000002e4
        // MSP       : 0x20003ff0
        // PSP       : 0x00000000
        // XPSR      : 0x61000000
        // EXTRA     : 0x00000000
        // FPSCR     : 0x00000000

        let values: Vec<_> = [
            0xfffffecc, // R0
            0x00000001, // R1
            0x00000000, // R2
            0x40008140, // R3
            0x000f4240, // R4
            0xfffffec0, // R5
            0x00000000, // R6
            0x20003ff0, // R7
            0x00000000, // R8
            0x00000000, // R9
            0x00000000, // R10
            0x00000000, // R11
            0x5000050c, // R12
            0x20003ff0, // R13 (SP)
            0x00200000, // R14 (RA)
            0x000002e4, // R15 (PC)
            0x20003ff0, // MSP
            0x00000000, // PSP
            0x61000000, // XPSR
        ]
        .into_iter()
        .enumerate()
        .map(|(id, r)| DebugRegister {
            dwarf_id: Some(id as u16),
            core_register: CORTEX_M_CORE_REGISTERS.core_register(id),
            value: Some(RegisterValue::U32(r)),
        })
        .collect();

        let regs = DebugRegisters(values);

        let mut dummy_mem = MockMemory::new();

        // Stack:
        // 0x20003ff0 = 0x20003ff8
        // 0x20003ff4 = 0x00000161
        // 0x20003ff8 = 0x00000000
        // 0x20003ffc = 0x0000013d

        dummy_mem.add_word_range(
            0x2000_3ff0,
            &[0x20003ff8, 0x00000161, 0x00000000, 0x0000013d],
        );

        let exception_handler = Box::new(ArmV7MExceptionHandler);

        let frames = debug_info
            .unwind(
                &mut dummy_mem,
                regs,
                exception_handler.as_ref(),
                Some(probe_rs_target::InstructionSet::Thumb2),
                500,
            )
            .unwrap();

        let printed_backtrace = frames
            .into_iter()
            .map(|f| TestFormatter(&f).to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }

    #[test]
    fn test_print_stacktrace() {
        let elf = Path::new("./tests/gpio-hal-blinky/elf");
        let coredump = include_bytes!("../tests/gpio-hal-blinky/coredump");

        let mut adapter = CoreDump::load_raw(coredump).unwrap();
        let debug_info = DebugInfo::from_file(elf).unwrap();

        let initial_registers = DebugRegisters::from_coredump(&adapter);
        let exception_handler = exception_handler_for_core(adapter.core_type());
        let instruction_set = adapter.instruction_set();

        let stack_frames = debug_info
            .unwind(
                &mut adapter,
                initial_registers,
                exception_handler.as_ref(),
                Some(instruction_set),
                1000,
            )
            .unwrap();

        let printed_backtrace = stack_frames
            .into_iter()
            .map(|f| TestFormatter(&f).to_string())
            .collect::<Vec<String>>()
            .join("");

        insta::assert_snapshot!(printed_backtrace);
    }

    /// A pointer graph can be cyclic, so the walk stops at a fixed depth.
    const MAX_VARIABLE_DEPTH: usize = 14;

    #[test_case("RP2040_full_unwind"; "full_unwind Armv6-m using RP2040")]
    #[test_case("RP2040_svcall"; "svcall Armv6-m using RP2040")]
    #[test_case("RP2040_systick"; "systick Armv6-m using RP2040")]
    #[test_case("nRF52833_xxAA_full_unwind"; "full_unwind Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_svcall"; "svcall Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_systick"; "systick Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_from_usagefault"; "hardfault_from_usagefault Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_from_busfault"; "hardfault_from_busfault Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_in_systick"; "hardfault_in_systick Armv7-m using nRF52833_xxAA")]
    #[test_case("stm32u585_nested_exceptions"; "nested exceptions Armv8-m using STM32U585")]
    #[test_case("stm32u585_hardfault_fp"; "hardfault frame pointer Armv8-m using STM32U585")]
    #[test_case("stm32u585_exception_no_debuginfo"; "exception handler without unwind info Armv8-m using STM32U585")]
    #[test_case("stm32u585_psp_exception"; "exception through a PSP->MSP transition Armv8-m using STM32U585")]
    #[test_case("atsamd51p19a"; "Armv7-em from C source code")]
    #[test_case("esp32c3_full_unwind"; "full_unwind RISC-V32E using esp32c3")]
    #[test_case("esp32s3_esp_hal_panic"; "Xtensa unwinding on an esp32s3 in a panic handler")]
    #[test_case("esp32c6_coredump_elf"; "Unwind using a RISC-V coredump in ELF format")]
    #[test_case("esp32s3_coredump_elf"; "Unwind using an Xtensa coredump in ELF format")]
    #[test_case("async_esp32s3"; "Unwind an async embassy task on an ESP32-S3")]
    #[test_case("async_esp32c6"; "Unwind an async embassy task on an ESP32-C6")]
    fn full_unwind(test_name: &str) {
        let debug_info =
            load_test_elf_as_debug_info(format!("debug-unwind-tests/{test_name}.elf").as_str());

        let coredump_path = coredump_path(format!("debug-unwind-tests/{test_name}"));
        let mut adapter = CoreDump::load(&coredump_path).unwrap();

        let snapshot_name = test_name.to_string();

        let initial_registers = DebugRegisters::from_coredump(&adapter);
        let exception_handler = exception_handler_for_core(adapter.core_type());
        let instruction_set = adapter.instruction_set();

        let mut stack_frames = debug_info
            .unwind(
                &mut adapter,
                initial_registers,
                exception_handler.as_ref(),
                Some(instruction_set),
                1000,
            )
            .unwrap();

        // Expand and validate the static and local variables for each stack frame.
        for i in 0..stack_frames.len() {
            let caller_registers = stack_frames.get(i + 1).map(|frame| frame.registers.clone());
            let caller_frame_base = stack_frames.get(i + 1).and_then(|frame| frame.frame_base);
            let caller_cfa = stack_frames
                .get(i + 1)
                .and_then(|frame| frame.canonical_frame_address);
            let caller = caller_registers.as_ref().map(|registers| StackFrameInfo {
                registers,
                frame_base: caller_frame_base,
                canonical_frame_address: caller_cfa,
                caller: None,
            });

            let frame = &mut stack_frames[i];
            let mut variable_caches = Vec::new();
            if let Some(local_variables) = &mut frame.local_variables {
                variable_caches.push(local_variables);
            }
            for variable_cache in variable_caches {
                // Cache the deferred top level children of the of the cache.
                variable_cache.recurse_deferred_variables(
                    &debug_info,
                    &mut adapter,
                    MAX_VARIABLE_DEPTH,
                    &StackFrameInfo {
                        registers: &frame.registers,
                        frame_base: frame.frame_base,
                        canonical_frame_address: frame.canonical_frame_address,
                        caller: caller.as_ref(),
                    },
                );
            }
        }

        // Using YAML output because it is easier to read than the default snapshot output,
        // and also because they provide better diffs.
        insta::assert_yaml_snapshot!(snapshot_name, stack_frames);
    }

    /// Expand every variable below `key` except a device register, and report how many device
    /// registers the walk left unexpanded.
    fn expand_around_registers(
        cache: &mut VariableCache,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        frame_info: &StackFrameInfo<'_>,
        key: ObjectRef,
        depth: usize,
    ) -> usize {
        let Some(mut variable) = cache.get_variable_by_key(key) else {
            return 0;
        };

        if variable.type_name.ident() == Some("VolatileCell") {
            // A read of the register flattens the wrapper away and replaces the value, so a
            // deferred node is the proof that the walk left the register alone.
            assert!(
                variable.variable_node_type.is_deferred(),
                "{:?} holds a device register, so the walk must not read it, but it read {:?}",
                variable.name,
                variable.value
            );
            return 1;
        }

        if depth == 0 {
            return 0;
        }

        if debug_info
            .cache_deferred_variables(cache, memory, &mut variable, frame_info)
            .is_err()
        {
            return 0;
        }

        let children: Vec<_> = cache.get_children(key).map(|c| c.variable_key).collect();
        children
            .into_iter()
            .map(|child| {
                expand_around_registers(cache, debug_info, memory, frame_info, child, depth - 1)
            })
            .sum()
    }

    #[test]
    fn a_device_register_does_not_expand_without_a_request() {
        let debug_info = load_test_elf_as_debug_info("debug-unwind-tests/async_esp32s3.elf");
        let coredump_path = coredump_path("debug-unwind-tests/async_esp32s3".to_string());
        let mut adapter = CoreDump::load(&coredump_path).unwrap();

        let initial_registers = DebugRegisters::from_coredump(&adapter);
        let exception_handler = exception_handler_for_core(adapter.core_type());
        let instruction_set = adapter.instruction_set();

        let mut stack_frames = debug_info
            .unwind(
                &mut adapter,
                initial_registers,
                exception_handler.as_ref(),
                Some(instruction_set),
                1000,
            )
            .unwrap();

        let mut registers = 0;
        for frame in &mut stack_frames {
            let frame_info = StackFrameInfo {
                registers: &frame.registers,
                frame_base: frame.frame_base,
                canonical_frame_address: frame.canonical_frame_address,
                caller: None,
            };
            let Some(cache) = &mut frame.local_variables else {
                continue;
            };
            let root = cache.root_variable().variable_key;
            registers += expand_around_registers(
                cache,
                &debug_info,
                &mut adapter,
                &frame_info,
                root,
                MAX_VARIABLE_DEPTH,
            );
        }

        assert!(registers > 0, "the test found no device register to check");
    }

    #[test_case("RP2040_full_unwind"; "Armv6-m using RP2040")]
    #[test_case("nRF52833_xxAA_full_unwind"; "Armv7-m using nRF52833_xxAA")]
    #[test_case("atsamd51p19a"; "Armv7-em from C source code")]
    // TODO:  #[test_case("esp32c3"; "RISC-V32E using esp32c3")]
    fn static_variables(chip_name: &str) {
        // TODO: Add RISC-V tests.

        let debug_info =
            load_test_elf_as_debug_info(format!("debug-unwind-tests/{chip_name}.elf").as_str());

        let coredump_path = coredump_path(format!("debug-unwind-tests/{chip_name}"));
        let mut adapter = CoreDump::load(&coredump_path).unwrap();

        let initial_registers = DebugRegisters::from_coredump(&adapter);

        let snapshot_name = format!("{chip_name}_static_variables");

        let mut static_variables = debug_info.create_static_scope_cache();

        static_variables.recurse_deferred_variables(
            &debug_info,
            &mut adapter,
            MAX_VARIABLE_DEPTH,
            &StackFrameInfo {
                registers: &initial_registers,
                frame_base: None,
                canonical_frame_address: None,
                caller: None,
            },
        );
        // Using YAML output because it is easier to read than the default snapshot output,
        // and also because they provide better diffs.
        insta::assert_yaml_snapshot!(snapshot_name, static_variables);
    }

    fn coredump_path(base: String) -> PathBuf {
        let possible_coredump_paths = [
            get_path_for_test_files(format!("{base}.coredump").as_str()),
            get_path_for_test_files(format!("{base}_coredump.elf").as_str()),
        ];

        possible_coredump_paths
            .iter()
            .find(|path| path.exists())
            .unwrap_or_else(|| {
                panic!(
                    "No coredump found for chip {base}. Expected one of: {possible_coredump_paths:?}"
                )
            })
            .clone()
    }

    #[test_case("/home/user/src/main.rs", true; "identical absolute path")]
    #[test_case("/home/user/./other/../src/main.rs", true; "unnormalized absolute path")]
    #[test_case("main.rs", true; "file name only")]
    #[test_case("src/main.rs", true; "partial sub path")]
    #[test_case("./src/main.rs", true; "unnormalized partial sub path")]
    #[test_case("ain.rs", false; "suffix that does not start at a component boundary")]
    #[test_case("/src/main.rs", false; "absolute path that is only a suffix")]
    #[test_case("src/lib.rs", false; "different file name")]
    #[test_case("/home/user/src", false; "prefix of the full path")]
    #[test_case("/home/other/src/main.rs", false; "different absolute path")]
    fn path_matches_unix(partial_path: &str, expected: bool) {
        let full_path = TypedPath::unix("/home/user/src/main.rs");

        assert_eq!(
            path_matches(full_path, TypedPath::unix(partial_path)),
            expected
        );
    }

    #[test_case(r"C:\Users\me\src\main.rs", true; "identical absolute path")]
    #[test_case(r"c:\users\me\SRC\Main.rs", true; "absolute path with different case")]
    #[test_case(r"src\main.rs", true; "partial sub path")]
    #[test_case(r"Main.rs", true; "file name with different case")]
    #[test_case(r"C:\Users\me\src\lib.rs", false; "different file name")]
    fn path_matches_windows(partial_path: &str, expected: bool) {
        let full_path = TypedPath::windows(r"C:\Users\me\src\main.rs");

        assert_eq!(
            path_matches(full_path, TypedPath::windows(partial_path)),
            expected
        );
    }

    /// DWARF info that a Windows compiler emitted can be inspected on a Unix host, and the other
    /// way around.
    #[test_case(TypedPath::windows(r"C:\Users\me\src\main.rs"), TypedPath::unix("src/main.rs"), true; "unix partial path against windows full path")]
    #[test_case(TypedPath::windows(r"C:\Users\me\src\main.rs"), TypedPath::unix("SRC/Main.rs"), true; "unix partial path with different case")]
    #[test_case(TypedPath::unix("/home/user/src/main.rs"), TypedPath::windows(r"src\main.rs"), true; "windows partial path against unix full path")]
    #[test_case(TypedPath::windows(r"C:\Users\me\src\main.rs"), TypedPath::unix("src/lib.rs"), false; "unix partial path with different file name")]
    fn path_matches_mixed_flavours(full_path: TypedPath, partial_path: TypedPath, expected: bool) {
        assert_eq!(path_matches(full_path, partial_path), expected);
    }
}
