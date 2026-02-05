use super::{
    DebugError, DebugRegisters, StackFrame, VariableCache,
    exception_handling::ExceptionInterface,
    function_die::{Die, FunctionDie},
    get_object_reference,
    unit_info::UnitInfo,
    variable::*,
};
use crate::{SourceLocation, VerifiedBreakpoint, stack_frame::StackFrameInfo, unit_info::RangeExt};
use gimli::{
    BaseAddresses, DebugFrame, RunTimeEndian, UnwindContext, UnwindSection, UnwindTableRow,
    read::RegisterRule,
};
use object::read::{Object, ObjectSection};
use probe_rs::{
    CoreRegister, Error, InstructionSet, MemoryInterface, RegisterDataType, RegisterRole,
    RegisterValue, UnwindRule,
};
use std::{
    borrow, cmp::Ordering, num::NonZeroU64, ops::ControlFlow, path::Path, rc::Rc, str::from_utf8,
};
use typed_path::{TypedPath, TypedPathBuf};

pub(crate) type GimliReader = gimli::EndianReader<RunTimeEndian, std::rc::Rc<[u8]>>;
pub(crate) type GimliReaderOffset =
    <gimli::EndianReader<RunTimeEndian, Rc<[u8]>> as gimli::Reader>::Offset;

pub(crate) type GimliAttribute = gimli::Attribute<GimliReader>;

pub(crate) type DwarfReader = gimli::read::EndianRcSlice<RunTimeEndian>;

/// Debug information which is parsed from DWARF debugging information.
pub struct DebugInfo {
    pub(crate) dwarf: gimli::Dwarf<DwarfReader>,
    pub(crate) frame_section: gimli::DebugFrame<DwarfReader>,
    pub(crate) locations_section: gimli::LocationLists<DwarfReader>,
    pub(crate) address_section: gimli::DebugAddr<DwarfReader>,
    pub(crate) debug_line_section: gimli::DebugLine<DwarfReader>,

    pub(crate) unit_infos: Vec<UnitInfo>,
    pub(crate) endianness: gimli::RunTimeEndian,

    pub(crate) addr2line: Option<addr2line::Loader>,
}

impl DebugInfo {
    /// Read debug info directly from a ELF file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<DebugInfo, DebugError> {
        let data = std::fs::read(path.as_ref())?;

        let mut this = DebugInfo::from_raw(&data)?;
        this.addr2line = addr2line::Loader::new(path).ok();
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

            Ok(gimli::read::EndianRcSlice::new(
                Rc::from(&*data),
                endianness,
            ))
        };

        // Load all of the sections.
        let dwarf_cow = gimli::Dwarf::load(&load_section)?;

        use gimli::Section;
        let mut frame_section = gimli::DebugFrame::load(load_section)?;
        let address_section = gimli::DebugAddr::load(load_section)?;
        let debug_loc = gimli::DebugLoc::load(load_section)?;
        let debug_loc_lists = gimli::DebugLocLists::load(load_section)?;
        let locations_section = gimli::LocationLists::new(debug_loc, debug_loc_lists);
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
            frame_section,
            locations_section,
            address_section,
            debug_line_section,
            unit_infos,
            endianness,
            addr2line: None,
        })
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

            while let Ok(Some(range)) = ranges.next() {
                if !(range.begin <= address && address < range.end) {
                    continue;
                }
                // Get the DWARF LineProgram.
                let ilnp = unit.line_program.as_ref()?.clone();

                let (program, sequences) = match ilnp.sequences() {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(
                            "No valid source code ranges found for address {}: {:?}",
                            address,
                            error
                        );
                        continue;
                    }
                };

                // Normalize the address.
                let mut target_seq = None;

                for seq in sequences {
                    if seq.start <= address && address < seq.end {
                        target_seq = Some(seq);
                        break;
                    }
                }

                let Some(target_seq) = target_seq.as_ref() else {
                    continue;
                };

                let mut previous_row: Option<gimli::LineRow> = None;

                let mut rows = program.resume_from(target_seq);

                while let Ok(Some((_, row))) = rows.next_row() {
                    match row.address().cmp(&address) {
                        Ordering::Greater => {
                            // The address is after the current row, so we use the previous row data.
                            //
                            // (If we don't do this, you get the artificial effect where the debugger
                            // steps to the top of the file when it is stepping out of a function.)
                            if let Some(previous_row) = previous_row
                                && let Some(path) =
                                    self.find_file_and_directory(unit, previous_row.file_index())
                            {
                                tracing::debug!("{:#010x} - {:?}", address, previous_row.isa());
                                return Some(SourceLocation {
                                    line: previous_row.line().map(NonZeroU64::get),
                                    column: Some(previous_row.column().into()),
                                    path,
                                    address: Some(previous_row.address()),
                                });
                            }
                        }
                        Ordering::Less => {}
                        Ordering::Equal => {
                            if let Some(path) = self.find_file_and_directory(unit, row.file_index())
                            {
                                tracing::debug!("{:#010x} - {:?}", address, row.isa());

                                return Some(SourceLocation {
                                    line: row.line().map(NonZeroU64::get),
                                    column: Some(row.column().into()),
                                    path,
                                    address: Some(row.address()),
                                });
                            }
                        }
                    }
                    previous_row = Some(*row);
                }
            }
        }
        None
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

    /// This effects the on-demand expansion of lazy/deferred load of all the 'child' `Variable`s for a given 'parent'.
    #[tracing::instrument(level = "trace", skip_all, fields(parent_variable = ?parent_variable.variable_key()))]
    pub fn cache_deferred_variables(
        &self,
        cache: &mut VariableCache,
        memory: &mut dyn MemoryInterface,
        parent_variable: &mut Variable,
        frame_info: StackFrameInfo<'_>,
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
            VariableNodeType::TypeOffset(header_offset, unit_offset)
            | VariableNodeType::DirectLookup(header_offset, unit_offset) => {
                let Some(unit_info) = self.unit_infos.iter().find(|unit_info| {
                    unit_info
                        .unit
                        .header
                        .offset()
                        .to_debug_info_offset(&unit_info.unit)
                        == Some(header_offset)
                }) else {
                    return Err(DebugError::Other(
                        "Failed to find unit info for offset lookup.".to_string(),
                    ));
                };

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
                }
            }
            _ => {
                // Do nothing. These have already been recursed to their maximum.
            }
        }
        Ok(())
    }

    /// Best-effort way to look up a function name without debuginfo.
    fn get_stackframe_from_symbols(
        &self,
        address: u64,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let Some(ref addr2line) = self.addr2line else {
            return Ok(vec![]);
        };
        let Some(fn_name) = addr2line.find_symbol(address) else {
            return Ok(vec![]);
        };

        let mut fn_name = fn_name.to_string();
        for lang in [
            gimli::DW_LANG_Rust,
            gimli::DW_LANG_C_plus_plus,
            gimli::DW_LANG_C_plus_plus_03,
            gimli::DW_LANG_C_plus_plus_11,
            gimli::DW_LANG_C_plus_plus_14,
        ] {
            if let Some(demangle) = addr2line::demangle(&fn_name, lang) {
                fn_name = demangle;
                break;
            }
        }

        Ok(vec![StackFrame {
            id: get_object_reference(),
            function_name: format!(
                "{fn_name} @ {address:#0width$x}>",
                width = (unwind_registers.get_address_size_bytes() * 2 + 2)
            ),
            source_location: None,
            registers: unwind_registers.clone(),
            pc: RegisterValue::from(address),
            frame_base: None,
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: None,
        }])
    }

    /// Returns a populated (resolved) [`StackFrame`] struct.
    /// This function will also populate the `DebugInfo::VariableCache` with in scope `Variable`s for each `StackFrame`,
    /// while taking into account the appropriate strategy for lazy-loading of variables.
    pub(crate) fn get_stackframe_info(
        &self,
        memory: &mut impl MemoryInterface,
        address: u64,
        cfa: Option<u64>,
        unwind_registers: &DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        // When reporting the address, we format it as a hex string, with the width matching
        // the configured size of the datatype used in the `RegisterValue` address.
        let unknown_function = || {
            format!(
                "<unknown function @ {address:#0width$x}>",
                width = (unwind_registers.get_address_size_bytes() * 2 + 2)
            )
        };

        let Ok((unit_info, functions)) = self.get_function_dies(address) else {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        };
        if functions.is_empty() {
            // No function found at the given address.
            return self.get_stackframe_from_symbols(address, unwind_registers);
        }

        // The first function is the non-inlined function, and the rest are inlined functions.
        // The frame base only exists for the non-inlined function, so we can reuse it for all the inlined functions.
        let frame_base = functions[0].frame_base(
            self,
            memory,
            StackFrameInfo {
                registers: unwind_registers,
                frame_base: None,
                canonical_frame_address: cfa,
            },
        )?;

        let mut frames = Vec::new();

        // Handle all functions which contain further inlined functions. For
        // these functions, the location is the call site of the inlined function.
        for function_pair in functions.windows(2) {
            let function_die = &function_pair[0];
            let next_function = &function_pair[1];

            let function_name = function_die
                .function_name(self)
                .unwrap_or_else(unknown_function);

            tracing::debug!("UNWIND: Function name: {}", function_name);

            assert!(next_function.is_inline());

            // Calculate the call site for this function, so that we can use it later to create an additional 'callee' `StackFrame` from that PC.
            let address_size = unit_info.unit.header.address_size() as u64;

            let Some(next_function_low_pc) = next_function.low_pc() else {
                tracing::warn!(
                    "UNWIND: Unknown starting address for inlined function {}.",
                    function_name
                );
                continue;
            };

            if !(next_function_low_pc > address_size && next_function_low_pc < u32::MAX as u64) {
                tracing::warn!("UNWIND: Unknown call site for inlined function {function_name}.");
                continue;
            }

            // The first instruction of the inlined function is used as the call site
            let inlined_call_site = RegisterValue::from(next_function_low_pc);

            tracing::debug!(
                "UNWIND: Callsite for inlined function {:?}",
                next_function.function_name(self)
            );

            let inlined_caller_source_location = next_function.inline_call_location(self);

            tracing::debug!("UNWIND: Call site: {inlined_caller_source_location:?}");

            // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
            // Resolve the statics that belong to the compilation unit that this function is in.
            // Next, resolve and cache the function variables.
            let local_variables = self
                .create_function_scope_cache(function_die, unit_info)
                .inspect_err(|error| {
                    tracing::error!("Could not resolve function variables. {error}. Continuing...");
                })
                .ok();

            frames.push(StackFrame {
                id: get_object_reference(),
                function_name,
                source_location: inlined_caller_source_location,
                registers: unwind_registers.clone(),
                pc: inlined_call_site,
                frame_base,
                is_inlined: function_die.is_inline(),
                local_variables,
                canonical_frame_address: cfa,
            });
        }

        // Handle last function, which contains no further inlined functions
        // `unwrap`: Checked at beginning of loop, functions must contain at least one value
        #[expect(clippy::unwrap_used)]
        let last_function = functions.last().unwrap();

        let function_name = last_function
            .function_name(self)
            .unwrap_or_else(unknown_function);

        let function_location = self.get_source_location(address);

        // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
        // Resolve and cache the function variables.
        let local_variables =
            self.create_function_scope_cache(last_function, unit_info)
                .map_or_else(
                    |error| {
                        tracing::error!(
                            "Could not resolve function variables. {error}. Continuing...",
                        );
                        None
                    },
                    Some,
                );

        frames.push(StackFrame {
            id: get_object_reference(),
            function_name,
            source_location: function_location,
            registers: unwind_registers.clone(),
            pc: match unwind_registers.get_address_size_bytes() {
                4 => RegisterValue::U32(address as u32),
                8 => RegisterValue::U64(address),
                _ => RegisterValue::from(address),
            },
            frame_base,
            is_inlined: last_function.is_inline(),
            local_variables,
            canonical_frame_address: cfa,
        });

        Ok(frames)
    }

    /// Performs the logical unwind of the stack and returns a `Vec<StackFrame>`
    /// - The first 'StackFrame' represents the frame at the current PC (program counter), and ...
    /// - Each subsequent `StackFrame` represents the **previous or calling** `StackFrame` in the call stack.
    /// - The majority of the work happens in the `'unwind: while` loop, where each iteration
    ///   will create a `StackFrame` where possible, and update the `unwind_registers` to prepare for
    ///   the next iteration.
    ///
    /// The unwind loop will continue until we meet one of the following conditions:
    /// - We can no longer unwind a valid PC value to be used for the next frame.
    /// - We encounter a LR register value of 0x0 or 0xFFFFFFFF (Arm 'Reset' value for that register).
    /// - We can not intelligently calculate a valid LR register value from the other registers,
    ///   or the `gimli::RegisterRule` result is a value of 0x0.
    ///   Note: [DWARF](https://dwarfstd.org) 6.4.4 - CIE defines the return register address
    ///   used in the `gimli::RegisterRule` tables for unwind operations.
    ///   Theoretically, if we encounter a function that has `Undefined` `gimli::RegisterRule` for
    ///   the return register address, it means we have reached the bottom of the stack
    ///   OR the function is a 'no return' type of function.
    ///   I have found actual examples (e.g. local functions) where we get `Undefined` for register
    ///   rule when we cannot apply this logic.
    ///   Example 1: local functions in main.rs will have LR rule as `Undefined`.
    ///   Example 2: main()-> ! that is called from a trampoline will have a valid LR rule.
    /// - Similarly, certain error conditions encountered in `StackFrameIterator` will also break out of the unwind loop.
    ///
    /// Note: In addition to populating the `StackFrame`s, this function will also
    /// populate the `DebugInfo::VariableCache` with `Variable`s for available Registers
    /// as well as static and function variables.
    /// TODO: Separate logic for stackframe creation and cache population
    pub fn unwind(
        &self,
        core: &mut impl MemoryInterface,
        initial_registers: DebugRegisters,
        exception_handler: &dyn ExceptionInterface,
        instruction_set: Option<InstructionSet>,
        max_stack_frame_count: usize,
    ) -> Result<Vec<StackFrame>, Error> {
        self.unwind_impl(
            initial_registers,
            core,
            exception_handler,
            instruction_set,
            max_stack_frame_count,
        )
    }

    pub(crate) fn unwind_impl(
        &self,
        initial_registers: DebugRegisters,
        memory: &mut impl MemoryInterface,
        exception_handler: &dyn ExceptionInterface,
        instruction_set: Option<InstructionSet>,
        max_stack_frame_count: usize,
    ) -> Result<Vec<StackFrame>, Error> {
        let mut stack_frames = Vec::<StackFrame>::new();

        let mut unwind_context = Box::new(gimli::UnwindContext::new());

        let mut unwind_registers = initial_registers;

        // Unwind [StackFrame]'s for as long as we can unwind a valid PC value.
        'unwind: while let Some(frame_pc_register_value) =
            unwind_registers.get_program_counter().and_then(|pc| {
                if pc.is_zero() | pc.is_max_value() {
                    None
                } else {
                    pc.value
                }
            })
        {
            if stack_frames.len() >= max_stack_frame_count {
                tracing::warn!("Stopped unwinding the stack after {max_stack_frame_count} frames");
                break;
            }
            let frame_pc = frame_pc_register_value.try_into().map_err(|error| {
                let message = format!("Cannot convert register value for program counter to a 64-bit integer value: {error:?}");
                Error::Register(message)
            })?;

            // PART 1: Construct the `StackFrame`s for the current program counter.
            //
            //         Multiple stack frames can be constructed if we are inside inlined functions.
            tracing::trace!(
                "UNWIND: Will generate `StackFrame` for function at address (PC) {frame_pc_register_value:#}"
            );
            let unwind_info = get_unwind_info(&mut unwind_context, &self.frame_section, frame_pc);

            // Determining the frame base may need the CFA (Canonical Frame Address) to be calculated first.
            let cfa = unwind_info
                .as_ref()
                .ok()
                .and_then(|unwind_info| determine_cfa(&unwind_registers, unwind_info).ok())
                .flatten();

            // PART 1-a: Prepare the `StackFrame`s that holds the current frame information.
            let cached_stack_frames =
                match self.get_stackframe_info(memory, frame_pc, cfa, &unwind_registers) {
                    Ok(cached_stack_frames) => cached_stack_frames,
                    Err(e) => {
                        tracing::error!("UNWIND: Unable to complete `StackFrame` information: {e}");
                        // There is no point in continuing with the unwind, so let's get out of here.
                        break;
                    }
                };

            // Add the found stackframes to the list, in reverse order. `get_stackframe_info` returns the frames in
            // the order of the most recently called function last, but the stack frames should be
            // in the order of the most recently called function first.
            if !cached_stack_frames.is_empty() {
                for frame in cached_stack_frames.into_iter().rev() {
                    if frame.is_inlined {
                        tracing::trace!(
                            "UNWIND: Found inlined function - name={}, pc={}",
                            frame.function_name,
                            frame.pc
                        );
                    }
                    stack_frames.push(frame);
                }
            } else {
                // We have no valid code for the current frame, so we
                // construct a frame, using what information we have.
                stack_frames.push(StackFrame {
                    id: get_object_reference(),
                    function_name: format!(
                        "<unknown function @ {:#0width$x}>",
                        frame_pc,
                        width = (unwind_registers.get_address_size_bytes() * 2 + 2)
                    ),
                    source_location: self.get_source_location(frame_pc),
                    registers: unwind_registers.clone(),
                    pc: frame_pc_register_value,
                    frame_base: None,
                    is_inlined: false,
                    local_variables: None,
                    canonical_frame_address: None,
                });
            };

            // PART 2: Setup the registers for the next iteration (a.k.a. unwind previous frame, a.k.a. "callee", in the call stack).
            tracing::trace!("UNWIND - Preparing to unwind the registers for the previous frame.");

            // Because we will be updating the `unwind_registers` with previous frame unwind info,
            // we need to keep a copy of the current frame's registers that can be used to resolve [DWARF](https://dwarfstd.org) expressions.
            let callee_frame_registers = unwind_registers.clone();

            // PART 2-a: get the `gimli::FrameDescriptorEntry` for the program counter
            // and then the unwind info associated with this row.
            let unwind_info = match unwind_info {
                Ok(unwind_info) => {
                    tracing::trace!("UNWIND: Found unwind info for address {frame_pc:#010x}");
                    unwind_info
                }
                Err(err) => {
                    tracing::trace!(
                        "UNWIND: Unable to find unwind info for address {frame_pc:#010x}: {err}"
                    );
                    if let ControlFlow::Break(error) = exception_handler.unwind_without_debuginfo(
                        &mut unwind_registers,
                        frame_pc,
                        &stack_frames,
                        instruction_set,
                        memory,
                    ) {
                        if let Some(error) = error {
                            // This is not fatal, but we cannot continue unwinding beyond the current frame.
                            tracing::error!("{:?}", &error);
                            if let Some(first_frame) = stack_frames.first_mut() {
                                first_frame.function_name =
                                    format!("{} : ERROR : {error}", first_frame.function_name);
                            };
                        }
                        break 'unwind;
                    }

                    if callee_frame_registers == unwind_registers {
                        tracing::debug!("No change, preventing infinite loop");
                        break;
                    }
                    continue 'unwind;
                }
            };

            // PART 2-b: Unwind registers for the "previous/calling" frame.
            for debug_register in unwind_registers.0.iter_mut() {
                // The program counter is handled later
                if debug_register
                    .core_register
                    .register_has_role(RegisterRole::ProgramCounter)
                {
                    continue;
                }

                match unwind_register(
                    debug_register,
                    &callee_frame_registers,
                    unwind_info,
                    cfa,
                    memory,
                ) {
                    Err(error) => {
                        tracing::error!("{:?}", &error);
                        if let Some(first_frame) = stack_frames.last_mut() {
                            first_frame.function_name =
                                format!("{} : ERROR: {error}", first_frame.function_name);
                        };
                        break 'unwind;
                    }
                    Ok(val) => {
                        debug_register.value = val;
                    }
                };
            }

            // PART 3: Check if we entered the current frame from an exception handler.
            // - If we are at an exception handler frame:
            //   - Create a "handler" stackframe that can be inserted into the stack_frames list,
            //     instead of "unknown function @ address";
            //   - Overwrite the unwind registers with the exception context.
            // - If for some reason we cannot determine the exception context, we silently continue with the rest of the unwind.
            // At worst, the unwind will be able to unwind the stack to the frame of the most recent exception handler.
            if unwind_registers
                .get_return_address()
                .is_some_and(|ra| ra.value.is_some())
            {
                match exception_handler.exception_details(memory, &unwind_registers, self) {
                    Ok(Some(exception_info)) => {
                        tracing::trace!(
                            "UNWIND: Stack unwind reached an exception handler {}",
                            exception_info.description
                        );
                        unwind_registers = exception_info.handler_frame.registers.clone();
                        stack_frames.push(exception_info.handler_frame);
                        // We have everything we need to unwind the next frame in the stack.
                        continue 'unwind;
                    }
                    Ok(None) => {
                        tracing::trace!(
                            "UNWIND: No exception context found. Stack unwind will continue."
                        );
                    }
                    Err(e) => {
                        // TODO: Nicely print error with sources
                        let message = format!(
                            "UNWIND: Error while checking for exception context. The stack trace will not include the calling frames.\n{e:?}"
                        );
                        tracing::warn!("{message}");
                        stack_frames.push(StackFrame {
                            id: get_object_reference(),
                            function_name: message,
                            source_location: None,
                            registers: unwind_registers.clone(),
                            pc: frame_pc_register_value,
                            frame_base: None,
                            is_inlined: false,
                            local_variables: None,
                            canonical_frame_address: None,
                        });
                        break 'unwind;
                    }
                };
            }

            let unwound_return_address = unwind_registers
                .get_register_by_role(&RegisterRole::ReturnAddress)
                .ok()
                .and_then(|reg| reg.value);

            let program_counter = unwind_registers.get_program_counter_mut().unwrap();

            let Ok(current_pc) =
                callee_frame_registers.get_register_value_by_role(&RegisterRole::ProgramCounter)
            else {
                let error = "UNWIND: Tried to unwind return address value where current program counter is unknown.";
                tracing::error!("{error}");
                if let Some(first_frame) = stack_frames.last_mut() {
                    first_frame.function_name =
                        format!("{} : ERROR: {error}", first_frame.function_name);
                };
                break 'unwind;
            };
            // NOTE: PC = Value of the unwound LR, i.e. the first instruction after the one that called this function.
            // If both the LR and PC registers have undefined rules, this will prevent the unwind from continuing.
            let register_rule_str;
            (program_counter.value, register_rule_str) = unwound_return_address
                .and_then(|return_address| {
                    unwind_program_counter_register(return_address, current_pc, instruction_set)
                })
                .unzip();

            tracing::trace!(
                "UNWIND - {:>10}: Caller: {}\tCallee: {}\tRule: {}",
                program_counter.get_register_name(),
                program_counter.value.unwrap_or_default(),
                callee_frame_registers
                    .get_register(program_counter.core_register.id)
                    .and_then(|reg| reg.value)
                    .unwrap_or_default(),
                register_rule_str.unwrap_or("PC=(undefined) (dwarf Undefined)"),
            );

            if callee_frame_registers == unwind_registers {
                tracing::debug!("No change, preventing infinite loop");
                break;
            }
        }

        Ok(stack_frames)
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

    pub(crate) fn find_file_and_directory(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        file_index: u64,
    ) -> Option<TypedPathBuf> {
        let combined_path = self.get_path(unit, file_index)?;

        Some(combined_path)
    }

    // Return the compilation unit that contains the given address
    pub(crate) fn compile_unit_info(
        &self,
        address: u64,
    ) -> Result<&super::unit_info::UnitInfo, DebugError> {
        for header in &self.unit_infos {
            match self.dwarf.unit_ranges(&header.unit) {
                Ok(mut ranges) => {
                    while let Ok(Some(range)) = ranges.next() {
                        if range.contains(address) {
                            return Ok(header);
                        }
                    }
                }
                Err(_) => continue,
            };
        }
        Err(DebugError::WarnAndContinue {
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

    /// Look up the DIE reference for the given attribute, if it exists.
    pub(crate) fn resolve_die_reference<'debug_info, 'unit_info>(
        &'debug_info self,
        attribute: gimli::DwAt,
        die: &Die,
        unit_info: &'unit_info UnitInfo,
    ) -> Option<Die>
    where
        'unit_info: 'debug_info,
    {
        let attr = die.attr(attribute)?;

        self.resolve_die_reference_with_unit(attr, unit_info)
            .ok()
            .map(|(_, die)| die)
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
            gimli::AttributeValue::DebugInfoRef(offset) => {
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
            other_attribute_value => Err(DebugError::Other(format!(
                "Unimplemented attribute value {other_attribute_value:?}"
            ))),
        }
    }
}

/// Uses the [`TypedPathBuf::normalize`] function to normalize both paths before comparing them
pub(crate) fn canonical_path_eq(primary_path: TypedPath, secondary_path: TypedPath) -> bool {
    primary_path.normalize() == secondary_path.normalize()
}

/// Get a handle to the [`gimli::UnwindTableRow`] for this call frame, so that we can reference it to unwind register values.
pub fn get_unwind_info<'a>(
    unwind_context: &'a mut UnwindContext<GimliReaderOffset>,
    frame_section: &DebugFrame<DwarfReader>,
    frame_program_counter: u64,
) -> Result<&'a gimli::UnwindTableRow<GimliReaderOffset>, DebugError> {
    let transform_error = |error| {
        DebugError::Other(format!(
            "UNWIND: Error reading FrameDescriptorEntry at PC={frame_program_counter:x} : {error}"
        ))
    };

    let unwind_bases = BaseAddresses::default();

    let frame_descriptor_entry = frame_section
        .fde_for_address(
            &unwind_bases,
            frame_program_counter,
            DebugFrame::cie_from_offset,
        )
        .map_err(transform_error)?;

    frame_descriptor_entry
        .unwind_info_for_address(
            frame_section,
            &unwind_bases,
            unwind_context,
            frame_program_counter,
        )
        .map_err(transform_error)
}

/// Determines the CFA (canonical frame address) for the current [`gimli::UnwindTableRow`], using the current register values.
pub fn determine_cfa<R: gimli::ReaderOffset>(
    unwind_registers: &DebugRegisters,
    unwind_info: &UnwindTableRow<R>,
) -> Result<Option<u64>, Error> {
    let gimli::CfaRule::RegisterAndOffset { register, offset } = unwind_info.cfa() else {
        unimplemented!()
    };

    let reg_val = unwind_registers
        .get_register_by_dwarf_id(register.0)
        .and_then(|register| register.value);

    let cfa = match reg_val {
        None => {
            tracing::error!(
                "UNWIND: `StackFrameIterator` unable to determine the unwind CFA: Missing value of register {}",
                register.0
            );
            None
        }

        Some(reg_val) if reg_val.is_zero() => {
            // If we encounter this rule for CFA, it implies the scenario depends on a FP/frame pointer to continue successfully.
            // Therefore, if reg_val is zero (i.e. FP is zero), then we do not have enough information to determine the CFA by rule.
            tracing::trace!(
                "UNWIND: Stack unwind complete - The FP register value unwound to a value of zero."
            );
            None
        }

        Some(reg_val) => {
            let unwind_cfa = add_to_address(
                reg_val.try_into()?,
                *offset,
                unwind_registers.get_address_size_bytes(),
            );
            tracing::trace!(
                "UNWIND - CFA : {:#010x}\tRule: {:?}",
                unwind_cfa,
                unwind_info.cfa()
            );
            Some(unwind_cfa)
        }
    };

    Ok(cfa)
}

/// Unwind the program counter for the caller frame, using the LR value from the callee frame.
pub fn unwind_pc_without_debuginfo(
    unwind_registers: &mut DebugRegisters,
    _frame_pc: u64,
    instruction_set: Option<InstructionSet>,
) -> ControlFlow<Option<DebugError>> {
    // For non exception frames, we cannot do stack unwinding if we do not have debug info.
    // However, there is one case where we can continue. When the frame registers have a valid
    // return address/LR value, we can use the LR value to calculate the PC for the calling frame.
    // The current logic will then use that PC to get the next frame's unwind info, and if that exists,
    // we will be able to continue unwinding.
    // If the calling frame has no debug info, then the unwinding will end with that frame.
    let callee_frame_registers = unwind_registers.clone();
    let unwound_return_address: Option<RegisterValue> = unwind_registers
        .get_return_address()
        .and_then(|lr| lr.value);

    // This will update the program counter in the `unwind_registers` with the PC value calculated from the LR value.
    if let Some(calling_pc) = unwind_registers.get_program_counter_mut() {
        let Ok(current_pc) =
            callee_frame_registers.get_register_value_by_role(&RegisterRole::ProgramCounter)
        else {
            return ControlFlow::Break(
                Some(Error::Other(
                    "UNWIND: Tried to unwind return address value where current program counter is unknown.".to_string()
                ).into())
            );
        };
        // NOTE: PC = Value of the unwound LR, i.e. the first instruction after the one that called this function.
        // If both the LR and PC registers have undefined rules, this will prevent the unwind from continuing.
        (calling_pc.value, _) = unwound_return_address
            .and_then(|return_address| {
                unwind_program_counter_register(return_address, current_pc, instruction_set)
            })
            .unzip();
    }

    ControlFlow::Continue(())
}

/// A per_register unwind, applying register rules and updating the [`registers::DebugRegister`] value as appropriate, before returning control to the calling function.
pub fn unwind_register(
    debug_register: &super::DebugRegister,
    // The callee_frame_registers are used to lookup values and never updated.
    callee_frame_registers: &DebugRegisters,
    unwind_info: &gimli::UnwindTableRow<GimliReaderOffset>,
    unwind_cfa: Option<u64>,
    memory: &mut dyn MemoryInterface,
) -> Result<Option<RegisterValue>, Error> {
    // If we do not have unwind info, or there is no register rule, then use UnwindRule::Undefined.
    let register_rule = debug_register
        .dwarf_id
        .and_then(|register_position| unwind_info.register(gimli::Register(register_position)))
        .unwrap_or(RegisterRule::Undefined);

    unwind_register_using_rule(
        debug_register.core_register,
        callee_frame_registers,
        unwind_cfa,
        memory,
        register_rule,
    )
}

fn unwind_register_using_rule(
    debug_register: &CoreRegister,
    callee_frame_registers: &DebugRegisters,
    unwind_cfa: Option<u64>,
    memory: &mut dyn MemoryInterface,
    register_rule: gimli::RegisterRule<usize>,
) -> Result<Option<RegisterValue>, Error> {
    use gimli::read::RegisterRule;

    let mut register_rule_string = format!("{register_rule:?}");

    let new_value = match register_rule {
        RegisterRule::Undefined => {
            // In many cases, the DWARF has `Undefined` rules for variables like frame pointer, program counter, etc.,
            // so we hard-code some rules here to make sure unwinding can continue. If there is a valid rule, it will bypass these hardcoded ones.
            match &debug_register {
                fp if fp.register_has_role(RegisterRole::FramePointer) => {
                    register_rule_string = "FP=CFA (dwarf Undefined)".to_string();
                    unwind_cfa.map(|unwind_cfa| {
                        if fp.data_type == RegisterDataType::UnsignedInteger(32) {
                            RegisterValue::U32(unwind_cfa as u32 & !0b11)
                        } else {
                            RegisterValue::U64(unwind_cfa & !0b11)
                        }
                    })
                }
                sp if sp.register_has_role(RegisterRole::StackPointer) => {
                    // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section B.1.4.1: Treat bits [1:0] as `Should be Zero or Preserved`
                    // - Applying this logic to RISC-V has no adverse effects, since all incoming addresses are already 32-bit aligned.
                    register_rule_string = "SP=CFA (dwarf Undefined)".to_string();
                    unwind_cfa.map(|unwind_cfa| {
                        if sp.data_type == RegisterDataType::UnsignedInteger(32) {
                            RegisterValue::U32(unwind_cfa as u32 & !0b11)
                        } else {
                            RegisterValue::U64(unwind_cfa & !0b11)
                        }
                    })
                }
                lr if lr.register_has_role(RegisterRole::ReturnAddress) => {
                    let Ok(current_pc) = callee_frame_registers
                        .get_register_value_by_role(&RegisterRole::ProgramCounter)
                    else {
                        return Err(
                            Error::Other(
                                "UNWIND: Tried to unwind return address value where current program counter is unknown.".to_string()
                            )
                        );
                    };
                    let Some(current_lr) = callee_frame_registers
                        .get_register_by_role(&RegisterRole::ReturnAddress)
                        .ok()
                        .and_then(|lr| lr.value)
                    else {
                        return Err(
                            Error::Other(
                                "UNWIND: Tried to unwind return address value where current return address is unknown.".to_string()
                            )
                        );
                    };

                    let current_lr_value: u64 = current_lr.try_into()?;

                    if current_pc == current_lr_value & !0b1 {
                        // If the previous PC is the same as the half-word aligned current LR,
                        // we have no way of inferring the previous frames LR until we have the PC.
                        register_rule_string = "LR=Undefined (dwarf Undefined)".to_string();
                        None
                    } else {
                        // We can attempt to continue unwinding with the current LR value, e.g. inlined code.
                        register_rule_string = "LR=Current LR (dwarf Undefined)".to_string();
                        Some(current_lr)
                    }
                }
                pc if pc.register_has_role(RegisterRole::ProgramCounter) => {
                    unreachable!("The program counter is handled separately")
                }
                other_register => {
                    // If the the register rule was not specified, then we either carry the previous value forward,
                    // or we clear the register value, depending on the architecture and register type.
                    match other_register.unwind_rule {
                        UnwindRule::Preserve => {
                            register_rule_string = "Preserve".to_string();
                            callee_frame_registers
                                .get_register(other_register.id)
                                .and_then(|reg| reg.value)
                        }
                        UnwindRule::Clear => {
                            register_rule_string = "Clear".to_string();
                            None
                        }
                        UnwindRule::SpecialRule => {
                            // When no DWARF rules are available, and it is not a special register like PC, SP, FP, etc.,
                            // we will clear the value. It is possible it might have its value set later if
                            // exception frame information is available.
                            register_rule_string = "Clear (no unwind rules specified)".to_string();
                            None
                        }
                    }
                }
            }
        }

        RegisterRule::SameValue => callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value),

        RegisterRule::Offset(address_offset) => {
            // "The previous value of this register is saved at the address CFA+N where CFA is the current CFA value and N is a signed offset"
            let Some(unwind_cfa) = unwind_cfa else {
                return Err(Error::Other(
                    "UNWIND: Tried to unwind `RegisterRule` at CFA = None.".to_string(),
                ));
            };
            let address_size = callee_frame_registers.get_address_size_bytes();
            let previous_frame_register_address =
                add_to_address(unwind_cfa, address_offset, address_size);

            register_rule_string = format!("CFA {register_rule:?}");

            // TODO: This should be the size of the register, not the address size.
            let result = match address_size {
                4 => {
                    let mut buff = [0u8; 4];
                    memory
                        .read(previous_frame_register_address, &mut buff)
                        .map(|_| RegisterValue::U32(u32::from_le_bytes(buff)))
                }
                8 => {
                    let mut buff = [0u8; 8];
                    memory
                        .read(previous_frame_register_address, &mut buff)
                        .map(|_| RegisterValue::U64(u64::from_le_bytes(buff)))
                }
                _ => {
                    return Err(Error::Other(format!(
                        "UNWIND: Address size {address_size} not supported."
                    )));
                }
            };

            match result {
                Ok(register_value) => Some(register_value),
                Err(error) => {
                    tracing::error!(
                        "UNWIND: Rule: Offset {} from address {:#010x}",
                        address_offset,
                        unwind_cfa
                    );

                    return Err(Error::Other(format!(
                        "UNWIND: Failed to read value for register {} from address {} ({} bytes): {}",
                        debug_register,
                        RegisterValue::from(previous_frame_register_address),
                        4,
                        error
                    )));
                }
            }
        }
        // TODO: Implement the remainder of these `RegisterRule`s
        _ => unimplemented!(),
    };

    tracing::trace!(
        "UNWIND - {:>10}: Caller: {}\tCallee: {}\tRule: {}",
        debug_register,
        new_value.unwrap_or_default(),
        callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value)
            .unwrap_or_default(),
        register_rule_string,
    );
    Ok(new_value)
}

/// Helper function to determine the program counter value for the previous frame.
pub fn unwind_program_counter_register(
    return_address: RegisterValue,
    current_pc: u64,
    instruction_set: Option<InstructionSet>,
) -> Option<(RegisterValue, &'static str)> {
    if return_address.is_max_value() || return_address.is_zero() {
        tracing::debug!(
            "No reliable return address is available, so we cannot determine the program counter to unwind the previous frame."
        );
        return None;
    }

    const DEFAULT_REGISTER_RULE_STR: &str = "PC=(unwound LR) (dwarf Undefined)";

    match return_address {
        RegisterValue::U32(return_address) => {
            match instruction_set {
                Some(InstructionSet::Thumb2) => {
                    // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section A5.1.2:
                    //
                    // We have to clear the last bit to ensure the PC is half-word aligned. (on ARM architecture,
                    // when in Thumb state for certain instruction types will set the LSB to 1)
                    Some((
                        RegisterValue::U32((return_address - 2) & !0b1),
                        "PC=(unwound (LR - 2) & !0b1) (dwarf Undefined)",
                    ))
                }
                Some(InstructionSet::RV32C) => Some((
                    RegisterValue::U32(return_address - 2),
                    "PC=(unwound x1 - 2) (dwarf Undefined)",
                )),
                Some(InstructionSet::RV32) => Some((
                    RegisterValue::U32(return_address - 4),
                    "PC=(unwound x1 - 4) (dwarf Undefined)",
                )),
                Some(InstructionSet::Xtensa) => {
                    let upper_bits = (current_pc as u32) & 0xC000_0000;
                    Some((
                        RegisterValue::U32((return_address & 0x3FFF_FFFF | upper_bits) - 3),
                        "PC=(unwound x0 - 3) (dwarf Undefined)",
                    ))
                }
                _ => Some((
                    RegisterValue::U32(return_address),
                    DEFAULT_REGISTER_RULE_STR,
                )),
            }
        }
        RegisterValue::U64(return_address) => Some((
            RegisterValue::U64(return_address),
            DEFAULT_REGISTER_RULE_STR,
        )),
        RegisterValue::U128(_) => {
            tracing::warn!("128 bit address space not supported");
            None
        }
    }
}

/// Helper function to handle adding a signed offset to a [`RegisterValue`] address.
/// The numerical overflow is handled based on the byte size (`address_size_in_bytes` parameter  )
/// of the [`RegisterValue`], as opposed to just the datatype of the `address` parameter.
/// In the case of unwinding stack frame register values, it makes no sense to wrap,
/// because it will result in invalid register address reads.
/// Instead, when we detect over/underflow, we return an address value of 0x0,
/// which will trigger a graceful (and logged) end of a stack unwind.
fn add_to_address(address: u64, offset: i64, address_size_in_bytes: usize) -> u64 {
    match address_size_in_bytes {
        4 => {
            if offset >= 0 {
                (address as u32)
                    .checked_add(offset as u32)
                    .map(u64::from)
                    .unwrap_or(0x0)
            } else {
                (address as u32).saturating_sub(offset.unsigned_abs() as u32) as u64
            }
        }
        8 => {
            if offset >= 0 {
                address.checked_add(offset as u64).unwrap_or(0x0)
            } else {
                address.saturating_sub(offset.unsigned_abs())
            }
        }
        _ => {
            panic!(
                "UNWIND: Address size {address_size_in_bytes} not supported.  Please report this as a bug."
            );
        }
    }
}

#[cfg(test)]
mod test {
    use crate::{
        DebugInfo, DebugRegister, DebugRegisters,
        exception_handling::{
            armv6m::ArmV6MExceptionHandler, armv7m::ArmV7MExceptionHandler,
            exception_handler_for_core,
        },
        stack_frame::{StackFrameInfo, TestFormatter},
    };

    use gimli::RegisterRule;
    use probe_rs::{
        CoreDump, RegisterValue,
        architecture::arm::core::registers::cortex_m::{self, CORTEX_M_CORE_REGISTERS},
        test::MockMemory,
    };
    use std::path::{Path, PathBuf};
    use test_case::test_case;

    use super::unwind_register_using_rule;

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
            .unwind_impl(
                regs,
                &mut mocked_mem,
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
            .unwind_impl(
                regs,
                &mut dummy_mem,
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
            .unwind_impl(
                regs,
                &mut dummy_mem,
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
            .unwind_impl(
                regs,
                &mut dummy_mem,
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

    #[test_case("RP2040_full_unwind"; "full_unwind Armv6-m using RP2040")]
    #[test_case("RP2040_svcall"; "svcall Armv6-m using RP2040")]
    #[test_case("RP2040_systick"; "systick Armv6-m using RP2040")]
    #[test_case("nRF52833_xxAA_full_unwind"; "full_unwind Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_svcall"; "svcall Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_systick"; "systick Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_from_usagefault"; "hardfault_from_usagefault Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_from_busfault"; "hardfault_from_busfault Armv7-m using nRF52833_xxAA")]
    #[test_case("nRF52833_xxAA_hardfault_in_systick"; "hardfault_in_systick Armv7-m using nRF52833_xxAA")]
    #[test_case("atsamd51p19a"; "Armv7-em from C source code")]
    #[test_case("esp32c3_full_unwind"; "full_unwind RISC-V32E using esp32c3")]
    #[test_case("esp32s3_esp_hal_panic"; "Xtensa unwinding on an esp32s3 in a panic handler")]
    #[test_case("esp32c6_coredump_elf"; "Unwind using a RISC-V coredump in ELF format")]
    #[test_case("esp32s3_coredump_elf"; "Unwind using an Xtensa coredump in ELF format")]
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
        for frame in stack_frames.iter_mut() {
            let mut variable_caches = Vec::new();
            if let Some(local_variables) = &mut frame.local_variables {
                variable_caches.push(local_variables);
            }
            for variable_cache in variable_caches {
                // Cache the deferred top level children of the of the cache.
                variable_cache.recurse_deferred_variables(
                    &debug_info,
                    &mut adapter,
                    10,
                    StackFrameInfo {
                        registers: &frame.registers,
                        frame_base: frame.frame_base,
                        canonical_frame_address: frame.canonical_frame_address,
                    },
                );
            }
        }

        // Using YAML output because it is easier to read than the default snapshot output,
        // and also because they provide better diffs.
        insta::assert_yaml_snapshot!(snapshot_name, stack_frames);
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
            10,
            StackFrameInfo {
                registers: &initial_registers,
                frame_base: None,
                canonical_frame_address: None,
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

    #[test]
    fn unwind_same_value() {
        let rule = gimli::RegisterRule::SameValue;

        let mut callee_frame_registers = DebugRegisters::default();
        let debug_register = CORTEX_M_CORE_REGISTERS.core_registers().next().unwrap();

        let expected_value = Some(RegisterValue::U32(0x1234));

        callee_frame_registers.0.push(DebugRegister {
            core_register: debug_register,
            dwarf_id: Some(0),
            value: expected_value,
        });

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            debug_register,
            &callee_frame_registers,
            None,
            &mut memory,
            rule,
        )
        .unwrap();

        assert_eq!(value, expected_value);
    }

    #[test]
    fn unwind_offset() {
        let cfa = 0x1000;
        let offset = 4;
        let rule = gimli::RegisterRule::Offset(offset as i64);
        let expected_value = 0xcafe;

        let expected_register_value = Some(RegisterValue::U32(expected_value));

        let mut memory = MockMemory::new();
        memory.add_word_range(cfa + offset, &[expected_value]);

        let mut callee_frame_registers = DebugRegisters::default();
        let debug_register = CORTEX_M_CORE_REGISTERS.core_registers().next().unwrap();

        callee_frame_registers.0.push(DebugRegister {
            core_register: debug_register,
            dwarf_id: Some(0),
            value: None,
        });

        // This is necessary for the unwind code to determine the address size of the system
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::PC,
            dwarf_id: Some(15),
            value: Some(RegisterValue::U32(0x0)),
        });

        let value = unwind_register_using_rule(
            debug_register,
            &callee_frame_registers,
            Some(cfa),
            &mut memory,
            rule,
        )
        .unwrap();

        assert_eq!(value, expected_register_value);
    }

    #[test]
    fn unwind_undefined_for_frame_pointer() {
        let mut callee_frame_registers = DebugRegisters::default();
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::FP,
            dwarf_id: Some(7),
            value: Some(RegisterValue::U32(0x100)),
        });

        // This is necessary for the unwind code to determine the address size of the system
        callee_frame_registers.0.push(DebugRegister {
            core_register: &cortex_m::PC,
            dwarf_id: Some(15),
            value: Some(RegisterValue::U32(0x0)),
        });

        let cfa = 0x200;

        let mut memory = MockMemory::new();

        let value = unwind_register_using_rule(
            &cortex_m::FP,
            &callee_frame_registers,
            Some(cfa),
            &mut memory,
            RegisterRule::Undefined,
        )
        .unwrap();

        // If there is no rule defined for the frame pointer,
        // we assume that it is the same as the canonical frame address.
        assert_eq!(value, Some(RegisterValue::U32(0x200)));
    }
}
