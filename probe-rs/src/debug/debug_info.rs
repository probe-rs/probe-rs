use super::{
    function_die::FunctionDie, get_sequential_key, unit_info::UnitInfo, unit_info::UnitIter,
    variable::*, DebugError, DebugRegisters, SourceLocation, StackFrame, VariableCache,
};
use crate::{
    core::Core,
    debug::{registers, source_statement::SourceStatements},
    MemoryInterface, RegisterValue,
};
use ::gimli::{FileEntry, LineProgramHeader, UnwindContext};
use gimli::{BaseAddresses, ColumnType, DebugFrame, UnwindSection};
use object::read::{Object, ObjectSection};
use probe_rs_target::InstructionSet;
use registers::RegisterGroup;
use std::{
    borrow,
    cmp::Ordering,
    convert::TryInto,
    num::NonZeroU64,
    ops::ControlFlow,
    path::{Path, PathBuf},
    rc::Rc,
    str::from_utf8,
};

pub(crate) type GimliReader = gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>;

pub(crate) type GimliAttribute = gimli::Attribute<GimliReader>;

pub(crate) type DwarfReader = gimli::read::EndianRcSlice<gimli::LittleEndian>;

/// Debug information which is parsed from DWARF debugging information.
pub struct DebugInfo {
    pub(crate) dwarf: gimli::Dwarf<DwarfReader>,
    pub(crate) frame_section: gimli::DebugFrame<DwarfReader>,
    pub(crate) locations_section: gimli::LocationLists<DwarfReader>,
    pub(crate) address_section: gimli::DebugAddr<DwarfReader>,
    pub(crate) debug_line_section: gimli::DebugLine<DwarfReader>,
}

impl DebugInfo {
    /// Read debug info directly from a ELF file.
    pub fn from_file<P: AsRef<Path>>(path: P) -> Result<DebugInfo, DebugError> {
        let data = std::fs::read(path)?;

        DebugInfo::from_raw(&data)
    }

    /// Parse debug information directly from a buffer containing an ELF file.
    pub fn from_raw(data: &[u8]) -> Result<Self, DebugError> {
        let object = object::File::parse(data)?;

        // Load a section and return as `Cow<[u8]>`.
        let load_section = |id: gimli::SectionId| -> Result<DwarfReader, gimli::Error> {
            let data = object
                .section_by_name(id.name())
                .and_then(|section| section.uncompressed_data().ok())
                .unwrap_or_else(|| borrow::Cow::Borrowed(&[][..]));

            Ok(gimli::read::EndianRcSlice::new(
                Rc::from(&*data),
                gimli::LittleEndian,
            ))
        };

        // Load all of the sections.
        let dwarf_cow = gimli::Dwarf::load(&load_section)?;

        use gimli::Section;
        let frame_section = gimli::DebugFrame::load(load_section)?;
        let address_section = gimli::DebugAddr::load(load_section)?;
        let debug_loc = gimli::DebugLoc::load(load_section)?;
        let debug_loc_lists = gimli::DebugLocLists::load(load_section)?;
        let locations_section = gimli::LocationLists::new(debug_loc, debug_loc_lists);
        let debug_line_section = gimli::DebugLine::load(load_section)?;

        Ok(DebugInfo {
            dwarf: dwarf_cow,
            frame_section,
            locations_section,
            address_section,
            debug_line_section,
        })
    }

    /// Get the name of the function at the given address.
    ///
    /// If no function is found, `None` will be returend.
    ///
    /// ## Inlined functions
    /// Multiple nested inline functions could exist at the given address.
    /// This function will currently return the innermost function in that case.
    pub fn function_name(
        &self,
        address: u64,
        find_inlined: bool,
    ) -> Result<Option<String>, DebugError> {
        let mut units = self.dwarf.units();

        while let Some(unit_info) = self.get_next_unit_info(&mut units) {
            let mut functions = unit_info.get_function_dies(address, None, find_inlined)?;

            // Use the last functions from the list, this is the function which most closely
            // corresponds to the PC in case of multiple inlined functions.
            if let Some(die_cursor_state) = functions.pop() {
                let function_name = die_cursor_state.function_name();

                if function_name.is_some() {
                    return Ok(function_name);
                }
            }
        }

        Ok(None)
    }

    /// Try get the [`SourceLocation`] for a given address.
    pub fn get_source_location(&self, address: u64) -> Option<SourceLocation> {
        let mut units = self.dwarf.units();

        while let Ok(Some(header)) = units.next() {
            let unit = match self.dwarf.unit(header) {
                Ok(unit) => unit,
                Err(_) => continue,
            };

            match self.dwarf.unit_ranges(&unit) {
                Ok(mut ranges) => {
                    while let Ok(Some(range)) = ranges.next() {
                        if range.begin <= address && address < range.end {
                            // Get the function name.

                            let ilnp = match unit.line_program.as_ref() {
                                Some(ilnp) => ilnp,
                                None => return None,
                            };

                            match ilnp.clone().sequences() {
                                Ok((program, sequences)) => {
                                    // Normalize the address.
                                    let mut target_seq = None;

                                    for seq in sequences {
                                        if seq.start <= address && address < seq.end {
                                            target_seq = Some(seq);
                                            break;
                                        }
                                    }

                                    if let Some(target_seq) = target_seq.as_ref() {
                                        let mut previous_row: Option<gimli::LineRow> = None;

                                        let mut rows = program.resume_from(target_seq);

                                        while let Ok(Some((header, row))) = rows.next_row() {
                                            match row.address().cmp(&address) {
                                                Ordering::Greater => {
                                                    // The address is after the current row, so we use the previous row data. (If we don't do this, you get the artificial effect where the debugger steps to the top of the file when it is steppping out of a function.)
                                                    if let Some(previous_row) = previous_row {
                                                        if let Some(file_entry) =
                                                            previous_row.file(header)
                                                        {
                                                            if let Some((file, directory)) = self
                                                                .find_file_and_directory(
                                                                    &unit, header, file_entry,
                                                                )
                                                            {
                                                                tracing::debug!(
                                                                    "{} - {:?}",
                                                                    address,
                                                                    previous_row.isa()
                                                                );
                                                                return Some(SourceLocation {
                                                                    line: previous_row
                                                                        .line()
                                                                        .map(NonZeroU64::get),
                                                                    column: Some(
                                                                        previous_row
                                                                            .column()
                                                                            .into(),
                                                                    ),
                                                                    file,
                                                                    directory,
                                                                    low_pc: Some(
                                                                        target_seq.start as u32,
                                                                    ),
                                                                    high_pc: Some(
                                                                        target_seq.end as u32,
                                                                    ),
                                                                });
                                                            }
                                                        }
                                                    }
                                                }
                                                Ordering::Less => {}
                                                Ordering::Equal => {
                                                    if let Some(file_entry) = row.file(header) {
                                                        if let Some((file, directory)) = self
                                                            .find_file_and_directory(
                                                                &unit, header, file_entry,
                                                            )
                                                        {
                                                            tracing::debug!(
                                                                "{} - {:?}",
                                                                address,
                                                                row.isa()
                                                            );

                                                            return Some(SourceLocation {
                                                                line: row
                                                                    .line()
                                                                    .map(NonZeroU64::get),
                                                                column: Some(row.column().into()),
                                                                file,
                                                                directory,
                                                                low_pc: Some(
                                                                    target_seq.start as u32,
                                                                ),
                                                                high_pc: Some(
                                                                    target_seq.end as u32,
                                                                ),
                                                            });
                                                        }
                                                    }
                                                }
                                            }
                                            previous_row = Some(*row);
                                        }
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(
                                        "No valid source code ranges found for address {}: {:?}",
                                        address,
                                        error
                                    );
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "No valid source code ranges found for address {}: {:?}",
                        address,
                        error
                    );
                }
            }
        }
        None
    }

    pub(crate) fn get_units(&self) -> UnitIter {
        self.dwarf.units()
    }

    pub(crate) fn get_next_unit_info(&self, units: &mut UnitIter) -> Option<UnitInfo> {
        while let Ok(Some(header)) = units.next() {
            if let Ok(unit) = self.dwarf.unit(header) {
                return Some(UnitInfo {
                    debug_info: self,
                    unit,
                });
            };
        }
        None
    }

    /// We do not actually resolve the children of `[VariableName::StaticScope]` automatically, and only create the necessary header in the `VariableCache`.
    /// This allows us to resolve the `[VariableName::StaticScope]` on demand/lazily, when a user requests it from the debug client.
    /// This saves a lot of overhead when a user only wants to see the `[VariableName::LocalScope]` or `[VariableName::Registers]` while stepping through code (the most common use cases)
    pub(crate) fn create_static_scope_cache(
        &self,
        core: &mut Core<'_>,
        unit_info: &UnitInfo,
    ) -> Result<VariableCache, DebugError> {
        let mut static_variable_cache = VariableCache::new();

        // Only process statics for this unit header.
        let abbrevs = &unit_info.unit.abbreviations;
        // Navigate the current unit from the header down.
        if let Ok(mut header_tree) = unit_info.unit.header.entries_tree(abbrevs, None) {
            let unit_node = header_tree.root()?;
            let mut static_root_variable = Variable::new(
                unit_info.unit.header.offset().as_debug_info_offset(),
                Some(unit_node.entry().offset()),
            );
            static_root_variable.variable_node_type = VariableNodeType::DirectLookup;
            static_root_variable.name = VariableName::StaticScopeRoot;
            static_variable_cache.cache_variable(None, static_root_variable, core)?;
        }
        Ok(static_variable_cache)
    }

    /// Creates the unpopulated cache for `function` variables
    pub(crate) fn create_function_scope_cache(
        &self,
        core: &mut Core<'_>,
        die_cursor_state: &FunctionDie,
        unit_info: &UnitInfo,
    ) -> Result<VariableCache, DebugError> {
        let mut function_variable_cache = VariableCache::new();

        let abbrevs = &unit_info.unit.abbreviations;
        let mut tree = unit_info
            .unit
            .header
            .entries_tree(abbrevs, Some(die_cursor_state.function_die.offset()))?;
        let function_node = tree.root()?;

        let mut function_root_variable = Variable::new(
            unit_info.unit.header.offset().as_debug_info_offset(),
            Some(function_node.entry().offset()),
        );
        function_root_variable.variable_node_type = VariableNodeType::DirectLookup;
        function_root_variable.name = VariableName::LocalScopeRoot;
        function_variable_cache.cache_variable(None, function_root_variable, core)?;
        Ok(function_variable_cache)
    }

    /// This effects the on-demand expansion of lazy/deferred load of all the 'child' `Variable`s for a given 'parent'.
    pub fn cache_deferred_variables(
        &self,
        cache: &mut VariableCache,
        core: &mut Core<'_>,
        parent_variable: &mut Variable,
        stack_frame_registers: &DebugRegisters,
        frame_base: Option<u64>,
    ) -> Result<(), DebugError> {
        if !parent_variable.is_valid() {
            // Do nothing. The parent_variable.get_value() will already report back the debug_error value.
            return Ok(());
        }
        match parent_variable.variable_node_type {
            VariableNodeType::ReferenceOffset(reference_offset) => {
                // Only attempt this part if the parent is a pointer and we have not yet resolved the referenced children.
                if !cache.has_children(parent_variable)? {
                    if let Some(header_offset) = parent_variable.unit_header_offset {
                        let unit_header =
                            self.dwarf.debug_info.header_from_offset(header_offset)?;
                        let unit_info = UnitInfo {
                            debug_info: self,
                            unit: gimli::Unit::new(&self.dwarf, unit_header)?,
                        };
                        // Reference to a type, or an node.entry() to another type or a type modifier which will point to another type.
                        let mut type_tree = unit_info
                            .unit
                            .header
                            .entries_tree(&unit_info.unit.abbreviations, Some(reference_offset))?;
                        let referenced_node = type_tree.root()?;
                        let mut referenced_variable = cache.cache_variable(
                            Some(parent_variable.variable_key),
                            Variable::new(
                                unit_info.unit.header.offset().as_debug_info_offset(),
                                Some(referenced_node.entry().offset()),
                            ),
                            core,
                        )?;

                        match &parent_variable.name {
                                VariableName::Named(name) => {
                                    if name.starts_with("Some") {
                                        referenced_variable.name =
                                            VariableName::Named(name.replacen('&', "*", 1));
                                    } else {
                                        referenced_variable.name =
                                            VariableName::Named(format!("*{}", name));
                                    }
                                }
                                other => referenced_variable.name = VariableName::Named(format!("Error: Unable to generate name, parent variable does not have a name but is special variable {:?}", other)),
                            }

                        match &parent_variable.memory_location {
                            VariableLocation::Address(address) => {
                                // Now, retrieve the location by reading the adddress pointed to by the parent variable.
                                referenced_variable.memory_location = match core
                                    .read_word_32(*address)
                                {
                                    Ok(memory_location) => {
                                        VariableLocation::Address(memory_location as u64)
                                    }
                                    Err(error) => {
                                        tracing::error!("Failed to read referenced variable address from memory location {} : {}.", parent_variable.memory_location , error);
                                        VariableLocation::Error(format!("Failed to read referenced variable address from memory location {} : {}.", parent_variable.memory_location, error))
                                    }
                                };
                            }
                            other => {
                                referenced_variable.memory_location =
                                    VariableLocation::Unsupported(format!(
                                        "Location {:?} not supported for referenced variables.",
                                        other
                                    ));
                            }
                        }

                        referenced_variable = cache.cache_variable(
                            referenced_variable.parent_key,
                            referenced_variable,
                            core,
                        )?;

                        if referenced_variable.type_name == VariableType::Base("()".to_owned()) {
                            // Only use this, if it is NOT a unit datatype.
                            cache.remove_cache_entry(referenced_variable.variable_key)?;
                        } else {
                            unit_info.extract_type(
                                referenced_node,
                                parent_variable,
                                referenced_variable,
                                core,
                                stack_frame_registers,
                                frame_base,
                                cache,
                            )?;
                        }
                    }
                }
            }
            VariableNodeType::TypeOffset(type_offset) => {
                // Only attempt this if the children are not already resolved.
                if !cache.has_children(parent_variable)? {
                    if let Some(header_offset) = parent_variable.unit_header_offset {
                        let unit_header =
                            self.dwarf.debug_info.header_from_offset(header_offset)?;
                        let unit_info = UnitInfo {
                            debug_info: self,
                            unit: gimli::Unit::new(&self.dwarf, unit_header)?,
                        };
                        // Find the parent node
                        let mut type_tree = unit_info
                            .unit
                            .header
                            .entries_tree(&unit_info.unit.abbreviations, Some(type_offset))?;
                        let parent_node = type_tree.root()?;

                        // For process_tree we need to create a temporary parent that will later be eliminated with VariableCache::adopt_grand_children
                        // TODO: Investigate if UnitInfo::process_tree can be modified to use `&mut parent_variable`, then we would not need this temporary variable.
                        let mut temporary_variable = parent_variable.clone();
                        temporary_variable.variable_key = 0;
                        temporary_variable.parent_key = Some(parent_variable.variable_key);
                        temporary_variable = cache.cache_variable(
                            Some(parent_variable.variable_key),
                            temporary_variable,
                            core,
                        )?;

                        temporary_variable = unit_info.process_tree(
                            parent_node,
                            temporary_variable,
                            core,
                            stack_frame_registers,
                            frame_base,
                            cache,
                        )?;

                        cache.adopt_grand_children(parent_variable, &temporary_variable)?;
                    }
                }
            }
            VariableNodeType::DirectLookup => {
                // Only attempt this if the children are not already resolved.
                if !cache.has_children(parent_variable)? {
                    if let Some(header_offset) = parent_variable.unit_header_offset {
                        let unit_header =
                            self.dwarf.debug_info.header_from_offset(header_offset)?;
                        let unit_info = UnitInfo {
                            debug_info: self,
                            unit: gimli::Unit::new(&self.dwarf, unit_header)?,
                        };
                        // Find the parent node
                        let mut type_tree = unit_info.unit.header.entries_tree(
                            &unit_info.unit.abbreviations,
                            parent_variable.variable_unit_offset,
                        )?;

                        // For process_tree we need to create a temporary parent that will later be eliminated with VariableCache::adopt_grand_children
                        // TODO: Investigate if UnitInfo::process_tree can be modified to use `&mut parent_variable`, then we would not need this temporary variable.
                        let mut temporary_variable = parent_variable.clone();
                        temporary_variable.variable_key = 0;
                        temporary_variable.parent_key = Some(parent_variable.variable_key);
                        temporary_variable = cache.cache_variable(
                            Some(parent_variable.variable_key),
                            temporary_variable,
                            core,
                        )?;

                        let parent_node = type_tree.root()?;

                        temporary_variable = unit_info.process_tree(
                            parent_node,
                            temporary_variable,
                            core,
                            stack_frame_registers,
                            frame_base,
                            cache,
                        )?;

                        cache.adopt_grand_children(parent_variable, &temporary_variable)?;
                    }
                }
            }
            _ => {
                // Do nothing. These have already been recursed to their maximum.
            }
        }
        Ok(())
    }

    /// Returns a populated (resolved) [`StackFrame`] struct.
    /// This function will also populate the `DebugInfo::VariableCache` with in scope `Variable`s for each `StackFrame`, while taking into account the appropriate strategy for lazy-loading of variables.
    pub(crate) fn get_stackframe_info(
        &self,
        core: &mut Core<'_>,
        address: u64,
        unwind_registers: &registers::DebugRegisters,
    ) -> Result<Vec<StackFrame>, DebugError> {
        let mut units = self.get_units();

        let unknown_function = format!(
            "<unknown function @ {:#0width$x}>",
            address,
            width = (unwind_registers.get_address_size_bytes() * 2 + 2)
        );
        let stack_frame_registers = unwind_registers.clone();

        let mut frames = Vec::new();

        while let Some(unit_info) = self.get_next_unit_info(&mut units) {
            let functions =
                unit_info.get_function_dies(address, Some(&stack_frame_registers), true)?;

            if functions.is_empty() {
                continue;
            }

            // Handle all functions which contain further inlined functions. For
            // these functions, the location is the call site of the inlined function.
            for (index, function_die) in functions[0..functions.len() - 1].iter().enumerate() {
                let mut inlined_call_site: Option<RegisterValue> = None;
                let mut inlined_caller_source_location: Option<SourceLocation> = None;

                let function_name = function_die
                    .function_name()
                    .unwrap_or_else(|| unknown_function.clone());

                tracing::debug!("UNWIND: Function name: {}", function_name);

                let next_function = &functions[index + 1];

                assert!(next_function.is_inline());

                // Calculate the call site for this function, so that we can use it later to create an additional 'callee' `StackFrame` from that PC.
                let address_size = unit_info.unit.header.address_size() as u64;

                if next_function.low_pc > address_size && next_function.low_pc < u32::MAX.into() {
                    // The first instruction of the inlined function is used as the call site
                    inlined_call_site = Some(RegisterValue::from(next_function.low_pc));

                    tracing::debug!(
                        "UNWIND: Callsite for inlined function {:?}",
                        next_function.function_name()
                    );

                    inlined_caller_source_location = next_function.inline_call_location();
                }

                if let Some(inlined_call_site) = inlined_call_site {
                    tracing::debug!("UNWIND: Call site: {:?}", inlined_caller_source_location);

                    tracing::trace!("UNWIND: Function name: {}", function_name);

                    // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
                    // Resolve the statics that belong to the compilation unit that this function is in.
                    let static_variables = self
                        .create_static_scope_cache(core, &unit_info)
                        .map_or_else(
                            |error| {
                                tracing::error!(
                                    "Could not resolve static variables. {}. Continuing...",
                                    error
                                );
                                None
                            },
                            Some,
                        );

                    // Next, resolve and cache the function variables.
                    let local_variables = self
                        .create_function_scope_cache(core, function_die, &unit_info)
                        .map_or_else(
                            |error| {
                                tracing::error!(
                                    "Could not resolve function variables. {}. Continuing...",
                                    error
                                );
                                None
                            },
                            Some,
                        );

                    frames.push(StackFrame {
                        // MS DAP Specification requires the id to be unique accross all threads, so using  so using unique `Variable::variable_key` of the `stackframe_root_variable` as the id.
                        id: get_sequential_key(),
                        function_name,
                        source_location: inlined_caller_source_location,
                        registers: stack_frame_registers.clone(),
                        pc: inlined_call_site,
                        frame_base: function_die.frame_base,
                        is_inlined: function_die.is_inline(),
                        static_variables,
                        local_variables,
                    });
                } else {
                    tracing::warn!(
                        "UNWIND: Unknown call site for inlined function {}.",
                        function_name
                    );
                }
            }

            // Handle last function, which contains no further inlined functions
            //UNWRAP: Checked at beginning of loop, functions must contain at least one value
            #[allow(clippy::unwrap_used)]
            let last_function = functions.last().unwrap();

            let function_name = last_function
                .function_name()
                .unwrap_or_else(|| unknown_function.clone());

            let function_location = self.get_source_location(address);

            // Now that we have the function_name and function_source_location, we can create the appropriate variable caches for this stack frame.
            // Resolve the statics that belong to the compilation unit that this function is in.
            let static_variables = self
                .create_static_scope_cache(core, &unit_info)
                .map_or_else(
                    |error| {
                        tracing::error!(
                            "Could not resolve static variables. {}. Continuing...",
                            error
                        );
                        None
                    },
                    Some,
                );

            // Next, resolve and cache the function variables.
            let local_variables = self
                .create_function_scope_cache(core, last_function, &unit_info)
                .map_or_else(
                    |error| {
                        tracing::error!(
                            "Could not resolve function variables. {}. Continuing...",
                            error
                        );
                        None
                    },
                    Some,
                );

            frames.push(StackFrame {
                // MS DAP Specification requires the id to be unique accross all threads, so using  so using unique `Variable::variable_key` of the `stackframe_root_variable` as the id.
                id: get_sequential_key(),
                function_name,
                source_location: function_location,
                registers: stack_frame_registers.clone(),
                pc: match unwind_registers.get_address_size_bytes() {
                    4 => RegisterValue::U32(address as u32),
                    8 => RegisterValue::U64(address),
                    _ => RegisterValue::from(address),
                },
                frame_base: last_function.frame_base,
                is_inlined: last_function.is_inline(),
                static_variables,
                local_variables,
            });

            break;
        }

        if frames.is_empty() {
            Ok(vec![StackFrame {
                id: get_sequential_key(),
                function_name: unknown_function,
                source_location: self.get_source_location(address),
                registers: stack_frame_registers,
                pc: match unwind_registers.get_address_size_bytes() {
                    4 => RegisterValue::U32(address as u32),
                    8 => RegisterValue::U64(address),
                    _ => RegisterValue::from(address),
                },
                frame_base: None,
                is_inlined: false,
                static_variables: None,
                local_variables: None,
            }])
        } else {
            Ok(frames)
        }
    }

    /// Performs the logical unwind of the stack and returns a `Vec<StackFrame>`
    /// - The first 'StackFrame' represents the frame at the current PC (program counter), and ...
    /// - Each subsequent `StackFrame` represents the **previous or calling** `StackFrame` in the call stack.
    /// - The majority of the work happens in the `'unwind: while` loop, where each iteration will create a `StackFrame` where possible, and update the `unwind_registers` to prepare for the next iteration.
    ///
    /// The unwind loop will continue until we meet one of the following conditions:
    /// - We can no longer unwind a valid PC value to be used for the next frame.
    /// - We encounter a LR register value of 0x0 or 0xFFFFFFFF(Arm 'Reset' value for that register).
    /// - TODO: Catch the situation where the PC value indicates a hard-fault or other non-recoverable exception
    /// - We can not intelligently calculate a valid LR register value from the other registers, or the gimli::RegisterRule result is a value of 0x0. Note: [DWARF](https://dwarfstd.org) 6.4.4 - CIE defines the return register address used in the `gimli::RegisterRule` tables for unwind operations. Theoretically, if we encounter a function that has `Undefined` `gimli::RegisterRule` for the return register address, it means we have reached the bottom of the stack OR the function is a 'no return' type of function. I have found actual examples (e.g. local functions) where we get `Undefined` for register rule when we cannot apply this logic. Example 1: local functions in main.rs will have LR rule as `Undefined`. Example 2: main()-> ! that is called from a trampoline will have a valid LR rule.
    /// - Similarly, certain error conditions encountered in `StackFrameIterator` will also break out of the unwind loop.
    /// Note: In addition to populating the `StackFrame`s, this function will also populate the `DebugInfo::VariableCache` with `Variable`s for available Registers as well as static and function variables.
    /// TODO: Separate logic for stackframe creation and cache population
    pub fn unwind(&self, core: &mut Core, address: u64) -> Result<Vec<StackFrame>, crate::Error> {
        let mut stack_frames = Vec::<StackFrame>::new();
        let mut unwind_registers = registers::DebugRegisters::from_core(core);

        if unwind_registers
            .get_program_counter()
            .map_or_else(|| true, |pc| pc.value != Some(RegisterValue::U64(address)))
        {
            return Err(crate::Error::Other(anyhow::anyhow!("UNWIND: Attempting to perform an unwind for address: {:#018x}, which does not match the core register program counter.", address)));
        }

        let mut unwind_context: Box<UnwindContext<DwarfReader>> =
            Box::new(gimli::UnwindContext::new());

        // Unwind [StackFrame]'s for as long as we can unwind a valid PC value.
        'unwind: while let Some(frame_pc_register_value) = unwind_registers
            .get_program_counter()
            .and_then(|pc| pc.value)
        {
            // PART 1: Construct the `StackFrame` for the current pc.
            let frame_pc = frame_pc_register_value
                .try_into()
                .map_err(|error| crate::Error::Other(anyhow::anyhow!("Cannot convert register value for program counter to a 64-bit integeer value: {:?}", error)))?;
            tracing::trace!(
                "UNWIND: Will generate `StackFrame` for function at address (PC) {}",
                frame_pc,
            );

            //
            // PART 1-a: Prepare the `StackFrame` that holds the current frame information.
            let return_frame = match self.get_stackframe_info(core, frame_pc, &unwind_registers) {
                Ok(mut cached_stack_frames) => {
                    while cached_stack_frames.len() > 1 {
                        // If we encountered INLINED functions (all `StackFrames`s in this Vec, except for the last one, which is the containing NON-INLINED function), these are simply added to the list of stack_frames we return.
                        #[allow(clippy::unwrap_used)]
                        let inlined_frame = cached_stack_frames.pop().unwrap(); // unwrap is safe while .len() > 1
                        tracing::trace!(
                            "UNWIND: Found inlined function - name={}, pc={}",
                            inlined_frame.function_name,
                            inlined_frame.pc
                        );
                        stack_frames.push(inlined_frame);
                    }
                    if cached_stack_frames.len() == 1 {
                        // If there is only one stack frame, then it is a NON-INLINED function, and we will attempt to unwind further.
                        #[allow(clippy::unwrap_used)]
                        cached_stack_frames.pop().unwrap() // unwrap is safe for .len==1
                    } else {
                        // Obviously something has gone wrong and zero stackframes were returned in the vector.
                        tracing::error!("UNWIND: No `StackFrame` information: available");
                        // There is no point in continuing with the unwind, so let's get out of here.
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("UNWIND: Unable to complete `StackFrame` information: {}", e);
                    // There is no point in continuing with the unwind, so let's get out of here.
                    break;
                }
            };

            // Part 1-b: Check LR values to determine if we can continue unwinding.
            // TODO: ARM has special ranges of LR addresses to indicate fault conditions. We should check those also.
            if let Some(check_return_address) = unwind_registers.get_return_address() {
                if check_return_address.is_max_value() || check_return_address.is_zero() {
                    // When we encounter the starting (after reset) return address, we've reached the bottom of the stack, so no more unwinding after this.
                    stack_frames.push(return_frame);
                    tracing::trace!("UNWIND: Stack unwind complete - Reached the 'Reset' value of the LR register.");
                    break;
                }
            } else {
                // If the debug info rules result in a None return address, we cannot continue unwinding.
                stack_frames.push(return_frame);
                tracing::trace!("UNWIND: Stack unwind complete - LR register value is 'None.");
                break;
            }

            // PART 2: Setup the registers for the next iteration (a.k.a. unwind previous frame, a.k.a. "callee", in the call stack).
            tracing::trace!(
                "UNWIND - Preparing `StackFrameIterator` to unwind NON-INLINED function {:?} at {:?}",
                return_frame.function_name,
                return_frame.source_location
            );
            // PART 2-a: get the `gimli::FrameDescriptorEntry` for this address and then the unwind info associated with this row.
            match get_unwind_info(&mut unwind_context, &self.frame_section, frame_pc) {
                Ok(unwind_info) => {
                    // Because we will be updating the `unwind_registers` with previous frame unwind info, we need to keep a copy of the current frame's registers that can be used to resolve [DWARF](https://dwarfstd.org) expressions.
                    let callee_frame_registers = unwind_registers.clone();
                    // PART 2-b: Determine the CFA (canonical frame address) to use for this unwind row.
                    let unwind_cfa = match unwind_info.cfa() {
                        gimli::CfaRule::RegisterAndOffset { register, offset } => {
                            let reg_val = unwind_registers
                                .get_register_by_dwarf_id(register.0)
                                .and_then(|register| register.value);
                            match reg_val {
                                Some(reg_val) => {
                                    if reg_val.is_zero() {
                                        // If we encounter this rule for CFA, it implies the scenario depends on a FP/frame pointer to continue successfully.
                                        // Therefore, if reg_val is zero (i.e. FP is zero), then we do not have enough information to determine the CFA by rule.
                                        stack_frames.push(return_frame);
                                        tracing::trace!("UNWIND: Stack unwind complete - The FP register value unwound to a value of zero.");
                                        break;
                                    }
                                    let unwind_cfa = add_to_address(reg_val.try_into()?, *offset);
                                    tracing::trace!(
                                        "UNWIND - CFA : {:#010x}\tRule: {:?}",
                                        unwind_cfa,
                                        unwind_info.cfa()
                                    );
                                    Some(unwind_cfa)
                                }
                                None => {
                                    tracing::error!("UNWIND: `StackFrameIterator` unable to determine the unwind CFA: Missing value of register {}",register.0);
                                    stack_frames.push(return_frame);
                                    break;
                                }
                            }
                        }
                        gimli::CfaRule::Expression(_) => unimplemented!(),
                    };

                    // PART 2-c: Unwind registers for the "previous/calling" frame.
                    // We sometimes need to keep a copy of the LR value to calculate the PC. For both ARM, and RISCV, The LR will be unwound before the PC, so we can reference it safely.
                    let mut unwound_return_address: Option<RegisterValue> = None;
                    for debug_register in
                        unwind_registers.0.iter_mut().filter(|platform_register| {
                            matches!(
                                platform_register.group,
                                RegisterGroup::Base | RegisterGroup::Singleton
                            )
                            // We include platform registers, as well as the singletons, because on RISCV, the program counter is separate from the platform_registers
                        })
                    {
                        if unwind_register(
                            debug_register,
                            &callee_frame_registers,
                            Some(unwind_info),
                            unwind_cfa,
                            &mut unwound_return_address,
                            core,
                        )
                        .is_break()
                        {
                            stack_frames.push(return_frame);
                            break 'unwind;
                        };
                    }
                }
                Err(error) => {
                    // We cannot do stack unwinding if we do not have debug info. However, there is one case where we can continue. When the following conditions are met:
                    // 1. The current frame is the first frame in the stack, AND ...
                    // 2. The frame registers have a valid return address/LR value.
                    // If both these conditions are met, we can push the 'unknown function' to the list of stack frames, and use the LR value to calculate the PC for the calling frame.
                    // The current logic will then use that PC to get the next frame's unwind info, and if that exists, will be able to continue unwinding.
                    // If the calling frame has no debug info, then the unwindindg will end with that frame.
                    if stack_frames.is_empty() {
                        let callee_frame_registers = unwind_registers.clone();
                        let mut unwound_return_address: Option<RegisterValue> =
                            callee_frame_registers
                                .get_return_address()
                                .and_then(|lr| lr.value);
                        if let Some(calling_pc) = unwind_registers.get_program_counter_mut() {
                            if unwind_register(
                                calling_pc,
                                &callee_frame_registers,
                                None,
                                None,
                                &mut unwound_return_address,
                                core,
                            )
                            .is_break()
                            {
                                // We were not able to get a PC for the calling frame, so we cannot continue unwinding.
                                stack_frames.push(return_frame);
                                break 'unwind;
                            } else {
                                // The unwind registers were updated with the calling frame's PC, so we can continue unwinding.
                                stack_frames.push(return_frame);
                                continue 'unwind;
                            };
                        }
                    } else {
                        stack_frames.push(return_frame);
                        tracing::trace!("UNWIND: Stack unwind complete. No available debug info for program counter {}: {}", frame_pc, error);
                        break;
                    }
                }
            };
            stack_frames.push(return_frame);
        }

        Ok(stack_frames)
    }

    /// Find the program counter where a breakpoint should be set,
    /// given a source file, a line and optionally a column.
    pub fn get_breakpoint_location(
        &self,
        path: &Path,
        line: u64,
        column: Option<u64>,
    ) -> Result<(Option<u64>, Option<SourceLocation>), DebugError> {
        tracing::debug!(
            "Looking for breakpoint location for {}:{}:{}",
            path.display(),
            line,
            column
                .map(|c| c.to_string())
                .unwrap_or_else(|| "-".to_owned())
        );

        let mut unit_iter = self.dwarf.units();

        while let Some(unit_header) = self.get_next_unit_info(&mut unit_iter) {
            let unit = &unit_header.unit;

            if let Some(ref line_program) = unit.line_program {
                let header = line_program.header();

                for file_name in header.file_names() {
                    let combined_path = self.get_path(unit, header, file_name);

                    if combined_path.map(|p| p == path).unwrap_or(false) {
                        let mut rows = line_program.clone().rows();

                        while let Some((header, row)) = rows.next_row()? {
                            let row_path = row
                                .file(header)
                                .and_then(|file_entry| self.get_path(unit, header, file_entry));

                            if row_path.map(|p| p != path).unwrap_or(true) {
                                continue;
                            }

                            if let Some(cur_line) = row.line() {
                                if cur_line.get() == line {
                                    // The first match of the file and row will be used to build the SourceStatements, and then:
                                    // 1. If there is an exact column match, we will use the low_pc of the statement at that column and line.
                                    // 2. If there is no exact column match, we use the first available statement in the line.
                                    let source_statements =
                                        SourceStatements::new(self, &unit_header, row.address())?
                                            .statements;
                                    if let Some((halt_address, halt_location)) = source_statements
                                        .iter()
                                        .find(|statement| {
                                            statement.line == Some(cur_line)
                                                && column
                                                    .and_then(NonZeroU64::new)
                                                    .map(ColumnType::Column)
                                                    .map_or(false, |col| col == statement.column)
                                        })
                                        .map(|source_statement| {
                                            (
                                                Some(source_statement.low_pc()),
                                                line_program
                                                    .header()
                                                    .file(source_statement.file_index)
                                                    .and_then(|file_entry| {
                                                        self.find_file_and_directory(
                                                            &unit_header.unit,
                                                            line_program.header(),
                                                            file_entry,
                                                        )
                                                        .map(|(file, directory)| SourceLocation {
                                                            line: source_statement
                                                                .line
                                                                .map(std::num::NonZeroU64::get),
                                                            column: Some(
                                                                source_statement.column.into(),
                                                            ),
                                                            file,
                                                            directory,
                                                            low_pc: Some(
                                                                source_statement.low_pc() as u32
                                                            ),
                                                            high_pc: Some(
                                                                source_statement
                                                                    .instruction_range
                                                                    .end
                                                                    as u32,
                                                            ),
                                                        })
                                                    }),
                                            )
                                        })
                                    {
                                        return Ok((halt_address, halt_location));
                                    } else if let Some((halt_address, halt_location)) =
                                        source_statements
                                            .iter()
                                            .find(|statement| statement.line == Some(cur_line))
                                            .map(|source_statement| {
                                                (
                                                    Some(source_statement.low_pc()),
                                                    line_program
                                                        .header()
                                                        .file(source_statement.file_index)
                                                        .and_then(|file_entry| {
                                                            self.find_file_and_directory(
                                                                &unit_header.unit,
                                                                line_program.header(),
                                                                file_entry,
                                                            )
                                                            .map(|(file, directory)| {
                                                                SourceLocation {
                                                                    line: source_statement
                                                                        .line
                                                                        .map(
                                                                        std::num::NonZeroU64::get,
                                                                    ),
                                                                    column: Some(
                                                                        source_statement
                                                                            .column
                                                                            .into(),
                                                                    ),
                                                                    file,
                                                                    directory,
                                                                    low_pc: Some(
                                                                        source_statement.low_pc()
                                                                            as u32,
                                                                    ),
                                                                    high_pc: Some(
                                                                        source_statement
                                                                            .instruction_range
                                                                            .end
                                                                            as u32,
                                                                    ),
                                                                }
                                                            })
                                                        }),
                                                )
                                            })
                                    {
                                        return Ok((halt_address, halt_location));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        Err(DebugError::Other(anyhow::anyhow!(
            "No valid breakpoint information found for file: {:?}, line: {:?}, column: {:?}",
            path,
            line,
            column
        )))
    }

    /// Get the absolute path for an entry in a line program header
    pub(crate) fn get_path(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        header: &LineProgramHeader<DwarfReader>,
        file_entry: &FileEntry<DwarfReader>,
    ) -> Option<PathBuf> {
        let file_name_attr_string = self.dwarf.attr_string(unit, file_entry.path_name()).ok()?;
        let dir_name_attr_string = file_entry
            .directory(header)
            .and_then(|dir| self.dwarf.attr_string(unit, dir).ok());

        let name_path = Path::new(from_utf8(&file_name_attr_string).ok()?);

        let dir_path =
            dir_name_attr_string.and_then(|dir_name| from_utf8(&dir_name).ok().map(PathBuf::from));

        let mut combined_path = match dir_path {
            Some(dir_path) => dir_path.join(name_path),
            None => name_path.to_owned(),
        };

        if combined_path.is_relative() {
            let comp_dir = unit
                .comp_dir
                .as_ref()
                .map(|dir| from_utf8(dir))
                .transpose()
                .ok()?
                .map(PathBuf::from);

            if let Some(comp_dir) = comp_dir {
                combined_path = comp_dir.join(&combined_path);
            }
        }

        Some(combined_path)
    }

    pub(crate) fn find_file_and_directory(
        &self,
        unit: &gimli::read::Unit<DwarfReader>,
        header: &LineProgramHeader<DwarfReader>,
        file_entry: &FileEntry<DwarfReader>,
    ) -> Option<(Option<String>, Option<PathBuf>)> {
        let combined_path = self.get_path(unit, header, file_entry)?;

        let file_name = combined_path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());

        let directory = combined_path.parent().map(|p| p.to_path_buf());

        Some((file_name, directory))
    }
}

/// Get a handle to the [`gimli::UnwindTableRow`] for this call frame, so that we can reference it to unwind register values.
fn get_unwind_info<'a>(
    unwind_context: &'a mut Box<UnwindContext<DwarfReader>>,
    frame_section: &'a DebugFrame<DwarfReader>,
    frame_program_counter: u64,
) -> Result<&'a gimli::UnwindTableRow<DwarfReader, gimli::StoreOnHeap>, DebugError> {
    let unwind_bases = BaseAddresses::default();
    let frame_descriptor_entry = match frame_section.fde_for_address(
        &unwind_bases,
        frame_program_counter,
        gimli::DebugFrame::cie_from_offset,
    ) {
        Ok(frame_descriptor_entry) => frame_descriptor_entry,
        Err(error) => {
            return Err(DebugError::Other(anyhow::anyhow!(
                "UNWIND: Error reading FrameDescriptorEntry at PC={} : {}",
                frame_program_counter,
                error
            )));
        }
    };

    frame_descriptor_entry
        .unwind_info_for_address(
            frame_section,
            &unwind_bases,
            unwind_context,
            frame_program_counter,
        )
        .map_err(|error| {
            DebugError::Other(anyhow::anyhow!(
                "UNWIND: Error reading FrameDescriptorEntry at PC={} : {}",
                frame_program_counter,
                error
            ))
        })
}

/// A per_register unwind, applying register rules and updating the [`registers::DebugRegister`] value as appropriate, before returning control to the calling function.
fn unwind_register(
    debug_register: &mut super::DebugRegister,
    // The callee_frame_registers are used to lookup values and never updated.
    callee_frame_registers: &DebugRegisters,
    unwind_info: Option<&gimli::UnwindTableRow<DwarfReader, gimli::StoreOnHeap>>,
    unwind_cfa: Option<u64>,
    unwound_return_address: &mut Option<RegisterValue>,
    core: &mut Core,
) -> ControlFlow<(), ()> {
    use gimli::read::RegisterRule::*;
    // If we do not have unwind info, or there is no register rule, then use UnwindRule::Undefined.
    let register_rule = debug_register
        .dwarf_id
        .and_then(|register_position| {
            unwind_info.map(|unwind_info| unwind_info.register(gimli::Register(register_position)))
        })
        .unwrap_or(gimli::RegisterRule::Undefined);
    let mut register_rule_string = format!("{:?}", register_rule);
    let new_value = match register_rule {
        Undefined => {
            // In many cases, the DWARF has `Undefined` rules for variables like frame pointer, program counter, etc., so we hard-code some rules here to make sure unwinding can continue. If there is a valid rule, it will bypass these hardcoded ones.
            match &debug_register {
                fp if fp.id == fp.register_file.frame_pointer.id => {
                    register_rule_string = "FP=CFA (dwarf Undefined)".to_string();
                    callee_frame_registers
                        .get_frame_pointer()
                        .and_then(|fp| fp.value)
                }
                sp if sp.id == sp.register_file.stack_pointer.id => {
                    // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section B.1.4.1: Treat bits [1:0] as `Should be Zero or Preserved`
                    // - Applying this logic to RISCV has no adverse effects, since all incoming addresses are already 32-bit aligned.
                    register_rule_string = "SP=CFA (dwarf Undefined)".to_string();
                    unwind_cfa.map(|unwind_cfa| {
                        if sp.is_u32() {
                            RegisterValue::U32(unwind_cfa as u32 & !0b11)
                        } else {
                            RegisterValue::U64(unwind_cfa & !0b11)
                        }
                    })
                }
                lr if lr.id == lr.register_file.return_address.id => {
                    // This value is can only be used to determine the Undefined PC value. We have no way of inferring the previous frames LR until we have the PC.
                    register_rule_string = "LR=Unknown (dwarf Undefined)".to_string();
                    *unwound_return_address = lr.value;
                    None
                }
                pc if pc.id == pc.register_file.program_counter.id => {
                    // NOTE: PC = Value of the unwound LR, i.e. the first instruction after the one that called this function.
                    register_rule_string = "PC=(unwound LR) (dwarf Undefined)".to_string();
                    unwound_return_address.and_then(|return_address| {
                        if return_address.is_max_value() || return_address.is_zero() {
                            tracing::warn!("No reliable return address is available, so we cannot determine the program counter to unwind the previous frame.");
                            None
                        } else {
                            match return_address {
                                RegisterValue::U32(return_address) => {
                                    if matches!(core.instruction_set(), Ok(InstructionSet::Thumb2)) {
                                        // NOTE: [ARMv7-M Architecture Reference Manual](https://developer.arm.com/documentation/ddi0403/ee), Section A5.1.2: We have to clear the last bit to ensure the PC is half-word aligned. (on ARM architecture, when in Thumb state for certain instruction types will set the LSB to 1)
                                        register_rule_string = "PC=(unwound LR & !0b1) (dwarf Undefined)".to_string();
                                        Some(RegisterValue::U32(return_address  & !0b1))
                                    } else{
                                        Some(RegisterValue::U32(return_address))
                                    }
                                }
                                RegisterValue::U64(return_address) => {
                                    Some(RegisterValue::U64(return_address))
                                },
                                RegisterValue::U128(_) => {
                                    tracing::warn!("128 bit address space not supported");
                                    None
                                }
                            }
                        }
                    })
                }
                _ => {
                    // This will result in the register value being cleared for the previous frame.
                    None
                }
            }
        }
        SameValue => callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value),
        Offset(address_offset) => {
            // "The previous value of this register is saved at the address CFA+N where CFA is the current CFA value and N is a signed offset"
            if let Some(unwind_cfa) = unwind_cfa {
                let previous_frame_register_address = add_to_address(unwind_cfa, address_offset);
                let address_size = callee_frame_registers.get_address_size_bytes();
                register_rule_string = format!("CFA {:?}", register_rule);
                let result = match address_size {
                    4 => {
                        let mut buff = [0u8; 4];
                        core.read(previous_frame_register_address, &mut buff)
                            .map(|_| RegisterValue::U32(u32::from_le_bytes(buff)))
                    }
                    8 => {
                        let mut buff = [0u8; 8];
                        core.read(previous_frame_register_address, &mut buff)
                            .map(|_| RegisterValue::U64(u64::from_le_bytes(buff)))
                    }
                    _ => {
                        tracing::error!(
                            "UNWIND: Address size {} not supported.  Please report this as a bug.",
                            address_size
                        );
                        return ControlFlow::Break(());
                    }
                };

                match result {
                    Ok(register_value) => {
                        if debug_register.id == debug_register.register_file.return_address.id {
                            // We need to store this value to be used by the calculation of the PC.
                            *unwound_return_address = Some(register_value);
                        }
                        Some(register_value)
                    }
                    Err(error) => {
                        tracing::error!(
                            "UNWIND: Failed to read value for register {} from address {} ({} bytes): {}",
                            debug_register.name,
                            RegisterValue::from(previous_frame_register_address),
                            4,
                            error
                        );
                        tracing::error!(
                            "UNWIND: Rule: Offset {} from address {:#010x}",
                            address_offset,
                            unwind_cfa
                        );
                        return ControlFlow::Break(());
                    }
                }
            } else {
                tracing::error!("UNWIND: Tried to unwind `RegisterRule` at CFA = None. Please report this as a bug.");
                return ControlFlow::Break(());
            }
        }
        //TODO: Implement the remainder of these `RegisterRule`s
        _ => unimplemented!(),
    };
    debug_register.value = new_value;

    tracing::trace!(
        "UNWIND - {:>10}: Caller: {}\tCallee: {}\tRule: {}",
        debug_register.get_register_name(),
        debug_register.value.unwrap_or_default(),
        callee_frame_registers
            .get_register(debug_register.id)
            .and_then(|reg| reg.value)
            .unwrap_or_default(),
        register_rule_string,
    );
    ControlFlow::Continue(())
}

/// Helper function to handle adding a signed offset to a u64 address.
/// The result wraps, which matches previous behavior of using i64 operations and
/// casting to u32
fn add_to_address(address: u64, offset: i64) -> u64 {
    if offset >= 0 {
        address.wrapping_add(offset as u64)
    } else {
        address.wrapping_sub(offset.unsigned_abs())
    }
}
