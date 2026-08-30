use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

use super::{
    DebugError, DebugRegisters, EndianReader, SourceLocation, VariableCache, debug_info::*,
    extract_alignment, extract_byte_size, extract_file, extract_line, function_die::FunctionDie,
    variable::*,
};
use crate::{ObjectRef, language, stack_frame::StackFrameInfo};
use gimli::{
    AttributeValue, DebugInfoOffset, DebuggingInformationEntry, DwAt, EvaluationResult, Location,
    UnitOffset,
};
use probe_rs::MemoryInterface;

/// Replace `variable` with the copy the walk left in the `cache`.
fn refresh(variable: &mut Variable, cache: &VariableCache) -> Result<(), DebugError> {
    let key = variable.variable_key;
    *variable = cache
        .get_variable_by_key(key)
        .ok_or_else(|| DebugError::Other(format!("Failed to find variable {key:?}.")))?;
    Ok(())
}

/// The result of `UnitInfo::evaluate_expression()` is a memory location of a variable.
#[derive(Debug)]
pub(crate) enum ExpressionResult {
    Location(VariableLocation),
}

enum AttributesEntry {
    Found(DebuggingInformationEntry<GimliReader, usize>),
    Unsupported,
    NotFound,
}

/// A struct containing information about a single compilation unit.
pub struct UnitInfo {
    pub(crate) unit: gimli::Unit<GimliReader, usize>,
    dwarf_language: gimli::DwLang,
    language: Box<dyn language::ProgrammingLanguage + Send + Sync>,
    // A mapping from child die to parent die.
    parents: HashMap<UnitOffset, UnitOffset>,
    // Address => function DIE offset
    function_dies: Vec<(Range<u64>, UnitOffset)>,
    // Return PC => DW_TAG_call_site_parameter (DW_AT_location bytes, DW_AT_call_value)
    call_sites: HashMap<u64, Vec<CallSiteParameter>>,
}

mod walk;

impl UnitInfo {
    /// Create a new `UnitInfo` from a `gimli::Unit`.
    pub fn new(unit: gimli::Unit<GimliReader, usize>, dwarf: &gimli::Dwarf<GimliReader>) -> Self {
        let dwarf_language = if let Some(AttributeValue::Language(unit_language)) = unit
            .entry(unit.root_offset())
            .ok()
            .and_then(|root| root.attr_value(gimli::DW_AT_language))
        {
            unit_language
        } else {
            tracing::warn!("Unable to retrieve DW_AT_language attribute, assuming Rust.");
            gimli::DW_LANG_Rust
        };

        let mut this = Self {
            unit,
            dwarf_language,
            language: language::from_dwarf(dwarf_language),
            parents: HashMap::new(),
            function_dies: Vec::new(),
            call_sites: HashMap::new(),
        };

        this.process_unit(dwarf);

        this
    }

    fn process_unit(&mut self, dwarf: &gimli::Dwarf<GimliReader>) {
        let mut entries_cursor = self.unit.entries();

        let mut prev_offset = None;
        let mut previous_depth = entries_cursor.depth();
        let mut active_call_site: Option<(isize, u64)> = None;
        while let Ok(Some(current)) = entries_cursor.next_dfs() {
            let parent_offset = match current.depth() - previous_depth {
                1 => {
                    // Previous die is our parent.
                    prev_offset
                }
                x if x <= 0 => {
                    let walk_up = |mut levels| {
                        // If 0:  Previous die is a sibling, we have the same parent.
                        // If <0: Previous die is a child of one of our siblings. Trace back as many levels as needed, and grab the parent.
                        let mut cursor = prev_offset.map(|off| self.parents.get(&off).copied())?;
                        while levels != 0 {
                            cursor = cursor.map(|off| self.parents.get(&off).copied())?;
                            levels += 1;
                        }
                        cursor
                    };
                    walk_up(x)
                }
                _ => unreachable!("DFS algorithms never jump down multiple levels in the graph"),
            };

            if let Some(offset) = parent_offset {
                self.parents.insert(current.offset(), offset);
            }
            previous_depth = current.depth();
            prev_offset = Some(current.offset());

            // Cache the address ranges if this DIE is a function.
            if current.tag() == gimli::DW_TAG_subprogram
                && let Ok(Some(ranges)) = FunctionDie::function_ranges(current, self, dwarf)
            {
                for range in ranges {
                    self.function_dies.push((range, current.offset()));
                }
            }

            collect_call_site(
                current,
                dwarf,
                &self.unit,
                &mut active_call_site,
                &mut self.call_sites,
            );

            // TODO: assuming the ranges don't overlap, sort function dies by start address
        }
    }

    pub(crate) fn call_site_value(
        &self,
        return_pc: u64,
        location: &gimli::Expression<GimliReader>,
    ) -> Option<gimli::Expression<GimliReader>> {
        let location = expression_bytes(location)?;
        let params = self
            .call_sites
            .get(&return_pc)
            .or_else(|| self.call_sites.get(&(return_pc & !1)))?;
        params
            .iter()
            .find(|param| param.location == location)
            .map(|param| param.value.clone())
    }

    /// Retrieve the value of the `DW_AT_language` attribute of the compilation unit.
    ///
    /// In the unlikely event that we are unable to retrieve the language, we assume Rust.
    pub(crate) fn get_language(&self) -> gimli::DwLang {
        self.dwarf_language
    }

    pub(crate) fn debug_info_offset(&self) -> Result<DebugInfoOffset, DebugError> {
        self.unit.header.offset().to_debug_info_offset(&self.unit.header).ok_or_else(|| DebugError::Other(
            "Failed to convert unit header offset to debug info offset. This is a bug, please report it.".to_string()
        ))
    }

    /// Get the compilation unit DIEs for the function containing the given address.
    /// - The first entry in the vector will be the outermost function containing the address.
    /// - If the address is inlined, the innermost function will be the last entry in the vector.
    pub(crate) fn get_function_dies<'debug_info>(
        &'debug_info self,
        debug_info: &'debug_info super::DebugInfo,
        address: u64,
    ) -> Result<Vec<FunctionDie<'debug_info>>, DebugError> {
        tracing::trace!("Searching Function DIE for address {:#010x}", address);

        // TODO: assuming the ranges don't overlap, binary-search for the function DIE containing the address
        let Some((_, start_offset)) = self
            .function_dies
            .iter()
            .find(|(range, _)| range.contains(&address))
            .cloned()
        else {
            return Ok(vec![]);
        };

        let mut entries_cursor = self.unit.entries_at_offset(start_offset)?;
        while let Ok(Some(current)) = entries_cursor.next_dfs() {
            let Some(die) = FunctionDie::new(current.clone(), self, debug_info, address)? else {
                continue;
            };

            let mut functions = vec![die];
            tracing::debug!(
                "Found DIE: name={:?}",
                functions[0].function_name(debug_info)
            );

            tracing::debug!("Checking for inlined functions");
            let inlined_functions =
                self.find_inlined_functions(debug_info, address, current.offset())?;
            tracing::debug!(
                "{} inlined functions for address {:#010x}",
                inlined_functions.len(),
                address
            );

            functions.extend(inlined_functions);
            return Ok(functions);
        }
        Ok(vec![])
    }

    /// Check if the function located at the given offset contains inlined functions at the
    /// given address.
    pub(crate) fn find_inlined_functions<'abbrev>(
        &'abbrev self,
        debug_info: &'abbrev DebugInfo,
        address: u64,
        parent_offset: UnitOffset,
    ) -> Result<Vec<FunctionDie<'abbrev>>, DebugError> {
        // If we don't have any entries at our unit offset, return an empty vector.
        // This cursor starts at, and includes the entries for the non-inlined function at 'parent_offset'.
        let Ok(mut cursor) = self.unit.entries_at_offset(parent_offset) else {
            return Ok(vec![]);
        };

        // The abort depth is used to control navigation of `cursor.next_dfs()` tree that contains
        // the inlined functions for the current address.  It is set to the current depth when a
        // qualifying inlined function is found, and prevents the cursor from searching back up the
        // tree, for sibling branches.
        // This is a performance optimization only, and will not affect the correctness of the result.
        let mut abort_depth = cursor.depth();
        let mut functions = Vec::new();

        while let Ok(Some(current)) = cursor.next_dfs() {
            if current.offset() == parent_offset {
                // We only want children of the non-inlined function DIE at the given `parent_offset`.
                continue;
            }

            if current.depth() < abort_depth {
                // We have found all the inlined functions for the current address
                // so we can abort the search, before it starts searching other branches of the tree.
                break;
            }

            // Keep the current DIE only if it is an inlined function
            let Some(die) = FunctionDie::new(current.clone(), self, debug_info, address)? else {
                continue;
            };

            // Every time we find a qualifying inlined-function, we set the abort depth
            // to ensure the `cursor.next_dfs()` will be prevented from reversing the depth traversal to search for peers.
            abort_depth = current.depth();

            functions.push(die);
        }

        Ok(functions)
    }

    #[allow(clippy::too_many_arguments)]
    fn attributes_entry(
        &self,
        attr: DwAt,
        debug_info: &DebugInfo,
        tree_node: &DebuggingInformationEntry<GimliReader, usize>,
        parent_variable: &mut Variable,
        child_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<AttributesEntry, DebugError> {
        let Some(abstract_origin) = tree_node.attr(attr) else {
            return Ok(AttributesEntry::NotFound);
        };

        match abstract_origin.value() {
            AttributeValue::UnitRef(unit_ref) => {
                // The abstract origin is a reference to another DIE, so we need to resolve that,
                // but first we need to process the (optional) memory location using the current DIE.
                self.process_memory_location(
                    debug_info,
                    tree_node,
                    parent_variable,
                    child_variable,
                    memory,
                    frame_info,
                )?;

                Ok(AttributesEntry::Found(self.unit.entry(unit_ref)?))
            }
            other_attribute_value => {
                child_variable.set_value(VariableValue::Error(format!(
                    "Unimplemented: Attribute Value for {attr} {other_attribute_value:?}"
                )));
                Ok(AttributesEntry::Unsupported)
            }
        }
    }

    /// Limit the variable to `DW_AT_start_scope` of the definition DIE, or of the abstract origin
    /// or specification DIE when the definition has no start scope.
    fn apply_start_scope(
        &self,
        debug_info: &DebugInfo,
        tree_node: &gimli::DebuggingInformationEntry<GimliReader>,
        attributes_entry: Option<&gimli::DebuggingInformationEntry<GimliReader>>,
        child_variable: &mut Variable,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let die = if tree_node.attr(gimli::DW_AT_start_scope).is_some() {
            tree_node
        } else if let Some(entry) = attributes_entry {
            entry
        } else {
            return Ok(());
        };

        if self.pc_in_start_scope(debug_info, die, frame_info)? {
            return Ok(());
        }

        child_variable.memory_location = VariableLocation::Unavailable;
        child_variable.set_value(VariableValue::Error(
            "<value optimized away by compiler, out of scope, or dropped>".to_string(),
        ));
        Ok(())
    }

    fn pc_in_start_scope(
        &self,
        debug_info: &DebugInfo,
        die: &gimli::DebuggingInformationEntry<GimliReader>,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<bool, DebugError> {
        let Some(attr) = die.attr(gimli::DW_AT_start_scope) else {
            return Ok(true);
        };
        let Some(program_counter) = frame_info
            .registers
            .get_program_counter()
            .and_then(|reg| reg.value)
        else {
            return Ok(true);
        };
        let program_counter = program_counter.try_into()?;

        let value = attr.value();
        if let Ok(Some(mut ranges)) = debug_info.dwarf.attr_ranges(&self.unit, value.clone()) {
            return Ok(ranges.contains(program_counter));
        }

        let Some(offset) = value.udata_value() else {
            tracing::debug!("Unimplemented: DW_AT_start_scope value {value:?}");
            return Ok(true);
        };

        let Some(scope_begin) = self.enclosing_scope_begin(debug_info, die.offset()) else {
            return Ok(true);
        };

        Ok(start_scope_constant_is_active(
            program_counter,
            scope_begin,
            offset,
        ))
    }

    fn enclosing_scope_begin(&self, debug_info: &DebugInfo, die_offset: UnitOffset) -> Option<u64> {
        let mut offset = die_offset;
        loop {
            offset = self.parent_offset(offset)?;
            let entry = self.unit.entry(offset).ok()?;
            let mut ranges = debug_info.dwarf.die_ranges(&self.unit, &entry).ok()?;
            if let Ok(Some(range)) = ranges.next() {
                return Some(range.begin);
            }
        }
    }

    /// Walk the compilation unit and add every static variable it declares to the `cache`.
    pub(crate) fn collect_static_variables(
        &self,
        debug_info: &DebugInfo,
        unit_offset: UnitOffset,
        parent_key: ObjectRef,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        walk::Walker::new(debug_info, memory, cache, frame_info).statics(
            self,
            unit_offset,
            parent_key,
        )
    }

    fn ensure_namespace(
        &self,
        debug_info: &DebugInfo,
        cache: &mut VariableCache,
        parent_key: ObjectRef,
        namespace_offset: UnitOffset,
    ) -> Result<ObjectRef, DebugError> {
        let entry = self.unit.entry(namespace_offset)?;
        let variable_name = if let Ok(Some(name)) = extract_name(debug_info, &self.unit, &entry) {
            VariableName::Namespace(name)
        } else {
            VariableName::AnonymousNamespace
        };

        if let Some(existing) = cache.get_variable_by_name_and_parent(&variable_name, parent_key) {
            return Ok(existing.variable_key());
        }

        let mut namespace_variable = Variable::new(Some(self));
        namespace_variable.name = variable_name;
        namespace_variable.type_name = VariableType::Namespace;
        namespace_variable.memory_location = VariableLocation::Unavailable;
        cache.add_variable(parent_key, &mut namespace_variable)?;
        Ok(namespace_variable.variable_key())
    }

    /// Walk the DIE tree below `parent_node`, add every descendant `Variable` to the `cache`, and
    /// refresh `parent_variable` from the cache.
    pub(crate) fn process_tree(
        &self,
        debug_info: &DebugInfo,
        parent_node: gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        if !parent_variable.is_valid() {
            cache.update_variable(parent_variable)?;
            return Ok(());
        }

        cache.update_variable(parent_variable)?;
        let parent_key = parent_variable.variable_key;
        let offset = parent_node.entry().offset();
        walk::Walker::new(debug_info, memory, cache, frame_info).tree(self, offset, parent_key)?;
        refresh(parent_variable, cache)
    }

    /// Add the variable that `pointer` points at, and resolve its type.
    pub(crate) fn resolve_pointer_target(
        &self,
        debug_info: &DebugInfo,
        type_offset: UnitOffset,
        pointer: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        walk::Walker::new(debug_info, memory, cache, frame_info).pointer_target(
            self,
            type_offset,
            pointer,
        )?;
        refresh(pointer, cache)
    }

    /// Extract the range information for an array.
    ///
    /// This is expected to be contained in an entry with type `DW_TAG_subrange_type`,
    /// looking like this:
    ///
    /// ```text
    /// 0x00000133:     DW_TAG_subrange_type
    ///                   DW_AT_type    (0x00000024 "unsigned int")
    ///                   DW_AT_upper_bound (0x44)
    /// ```
    /// Note that there might be multiple ranges, so this function returns a vector of ranges.
    fn extract_array_range(
        &self,
        array_parent_node: UnitOffset,
    ) -> Result<Vec<Range<u64>>, DebugError> {
        let mut tree = self.unit.entries_tree(Some(array_parent_node))?;

        let root = tree.root()?;

        let mut children = root.children();

        let mut ranges = vec![];
        while let Some(child) = children.next()? {
            match child.entry().tag() {
                gimli::DW_TAG_subrange_type => {
                    if let Some(range) = self.extract_array_range_attribute(child.entry())? {
                        ranges.push(range);
                    }
                }
                other => tracing::debug!(
                    "Ignoring unexpected child tag {} while extracting array range",
                    other
                ),
            }
        }

        Ok(ranges)
    }

    /// Extract the array range values
    ///
    /// See [`extract_array_range()`](Self::extract_array_range()) for more information.
    fn extract_array_range_attribute(
        &self,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
    ) -> Result<Option<Range<u64>>, DebugError> {
        let mut lower_bound = None;
        let mut upper_bound = None;

        // Now loop through all the unit attributes to extract the remainder of the `Variable` definition.
        for attr in entry.attrs() {
            match attr.name() {
                // Property of variables that are of DW_TAG_subrange_type.
                gimli::DW_AT_lower_bound => match attr.value().udata_value() {
                    Some(bound) => lower_bound = Some(bound),
                    None => {
                        return Err(DebugError::Other(format!(
                            "Unimplemented: Attribute Value for DW_AT_lower_bound: {:?}",
                            attr.value()
                        )));
                    }
                },
                gimli::DW_AT_count => match attr.value().udata_value() {
                    Some(count) => upper_bound = Some(count),
                    None => {
                        return Err(DebugError::Other(format!(
                            "Unimplemented: Attribute Value for DW_AT_count: {:?}",
                            attr.value()
                        )));
                    }
                },
                gimli::DW_AT_upper_bound => {
                    match attr.value().udata_value() {
                        // Rust ranges are exclusive, but the DWARF upper bound is inclusive.
                        Some(bound) => upper_bound = Some(bound + 1),
                        None => {
                            return Err(DebugError::Other(format!(
                                "Unimplemented: Attribute Value for DW_AT_upper_bound: {:?}",
                                attr.value()
                            )));
                        }
                    }
                }
                // Some compilers specify the type of the array size, but we don't use this information
                // currently.
                gimli::DW_AT_type => (),
                other_attribute => {
                    tracing::debug!(
                        "Unimplemented: Ignoring attribute {} while extracting array range",
                        other_attribute,
                    );
                }
            }
        }

        if let Some(upper_bound) = upper_bound {
            Ok(Some(lower_bound.unwrap_or_default()..upper_bound))
        } else {
            Ok(None)
        }
    }

    /// Compute the discriminant value of a DW_TAG_variant variable. If it is not explicitly captured in the DWARF,
    /// then it is the default value.
    pub(crate) fn extract_variant_discriminant(
        &self,
        node: &gimli::EntriesTreeNode<GimliReader>,
        variable: &mut Variable,
    ) -> Result<(), DebugError> {
        variable.role = match node.entry().attr(gimli::DW_AT_discr_value) {
            Some(discr_value_attr) => {
                let attr_value = discr_value_attr.value();
                let variant = if let Some(const_value) = attr_value.udata_value() {
                    const_value
                } else {
                    variable.set_value(VariableValue::Error(format!(
                        "Unimplemented: Attribute Value for DW_AT_discr_value: {:.100}",
                        format!("{attr_value:?}")
                    )));
                    u64::MAX
                };

                VariantRole::Variant(variant)
            }
            None => {
                // In the case where the variable is a DW_TAG_variant, but has NO DW_AT_discr_value, then this is the
                // "default" to be used.
                VariantRole::Variant(u64::MAX)
            }
        };

        Ok(())
    }

    /// `true` if this structure DIE has members or a variant part.
    /// Language-specific rewrite after a struct's members are in the cache.
    pub(crate) fn process_struct(
        &self,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        self.language
            .process_struct(self, debug_info, node, variable, memory, cache, frame_info)
    }

    #[expect(clippy::too_many_arguments)]
    fn extract_enumeration_type(
        &self,
        child_variable: &mut Variable,
        type_name: Option<String>,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        memory: &mut dyn MemoryInterface,
        frame_info: &StackFrameInfo,
    ) -> Result<(), DebugError> {
        let mut visiting = HashSet::new();
        if let Some(offset) = node.offset().to_debug_info_offset(&self.unit.header) {
            visiting.insert(offset);
        }
        child_variable.type_name = VariableType::Enum(self.extract_named_type(
            debug_info,
            node,
            type_name.unwrap_or_else(|| "<unnamed enum>".to_string()),
            &mut visiting,
        ));

        self.process_memory_location(
            debug_info,
            node,
            parent_variable,
            child_variable,
            memory,
            frame_info,
        )?;

        let mut tree = self.unit.entries_tree(Some(node.offset()))?;
        let enumerator_values = self.process_enumerator(debug_info, tree.root()?)?;

        if !(parent_variable.is_valid() && child_variable.is_valid()) {
            return Ok(());
        }

        // Determine the underlying integer value of the enum from its location.
        // It may live in memory or, at -Og/-O0, directly in a register (the location evaluation
        // already carries the value).
        let byte_size = child_variable.byte_size.unwrap_or(1).clamp(1, 16) as usize;
        let this_enum_const_value = match child_variable.memory_location {
            VariableLocation::Address(address) => {
                let mut buff = [0u8; 16];
                memory.read(address, &mut buff[..byte_size])?;
                Some(u128::from_le_bytes(buff))
            }
            VariableLocation::RegisterValue(register_value) => {
                TryInto::<u128>::try_into(register_value)
                    .ok()
                    .map(|value| truncate(value, byte_size))
            }
            _ => None,
        };

        let value = match this_enum_const_value {
            Some(this_enum_const_value) => {
                // The enumerators may be signed or unsigned, so accept either reading.
                let as_signed = sign_extend(this_enum_const_value, byte_size);
                let unresolved;
                let enumerator_value = match enumerator_values.iter().find(|(_name, value)| {
                    let VariableValue::Valid(value) = value else {
                        return false;
                    };
                    value
                        .parse::<u128>()
                        .is_ok_and(|value| value == this_enum_const_value)
                        || value.parse::<i128>().is_ok_and(|value| value == as_signed)
                }) {
                    Some((name, _value)) => name,
                    None => {
                        unresolved = VariableName::Named(format!(
                            "<Error: Unresolved enum value {this_enum_const_value}>"
                        ));
                        &unresolved
                    }
                };

                self.language
                    .format_enum_value(&child_variable.type_name, enumerator_value)
            }
            None => VariableValue::Error(format!(
                "Unsupported variable location {:?}",
                child_variable.memory_location
            )),
        };

        child_variable.set_value(value);

        Ok(())
    }

    /// Extract the different variants of an enumeration
    ///
    /// This is used for C-style enums, where the enum is an integer type,
    /// and all the different variants are different integer values.
    fn process_enumerator(
        &self,
        debug_info: &DebugInfo,
        parent_node: gimli::EntriesTreeNode<GimliReader>,
    ) -> Result<Vec<(VariableName, VariableValue)>, DebugError> {
        let mut enumerator_values = Vec::new();

        let mut child_nodes = parent_node.children();
        while let Some(child_node) = child_nodes.next()? {
            match child_node.entry().tag() {
                gimli::DW_TAG_enumerator => {
                    let attributes_entry = child_node.entry();

                    let name_result = extract_name(debug_info, &self.unit, attributes_entry);

                    let Some(attr_value) = attributes_entry.attr_value(gimli::DW_AT_const_value)
                    else {
                        // Ignore enumerators without a value.
                        continue;
                    };
                    let variable_value = if let Some(const_value) = attr_value.udata_value() {
                        VariableValue::Valid(const_value.to_string())
                    } else if let Some(const_value) = attr_value.sdata_value() {
                        VariableValue::Valid(const_value.to_string())
                    } else {
                        VariableValue::Error(format!(
                            "Unimplemented: Attribute Value for DW_AT_const_value: {attr_value:?}"
                        ))
                    };

                    let enumerator_name = if let Ok(Some(ref name)) = name_result {
                        name.to_string()
                    } else {
                        tracing::warn!("Enumerator has no name");

                        format!("<unknown enumerator {}", enumerator_values.len())
                    };

                    enumerator_values.push((VariableName::Named(enumerator_name), variable_value))
                }
                // Function implemented on the enum type, ignored here.
                gimli::DW_TAG_subprogram => (),
                other => {
                    tracing::debug!("Ignoring tag {other} under DW_TAG_enumeration_type");
                }
            }
        }

        Ok(enumerator_values)
    }

    /// Create child variable entries to represent array members and their values.
    #[expect(
        clippy::too_many_arguments,
        reason = "the public signature matches the existing call sites"
    )]
    pub(crate) fn expand_array_members(
        &self,
        debug_info: &DebugInfo,
        array_member_type_node: &DebuggingInformationEntry<GimliReader>,
        cache: &mut VariableCache,
        array_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        subranges: &[Range<u64>],
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        cache.update_variable(array_variable)?;
        walk::Walker::new(debug_info, memory, cache, frame_info).array_members(
            self,
            array_member_type_node.offset(),
            array_variable.variable_key,
            subranges.to_vec(),
        )?;
        refresh(array_variable, cache)
    }

    /// Process a memory location for a variable, by first evaluating the `byte_size`, and then calling the `self.extract_location`.
    pub(crate) fn process_memory_location(
        &self,
        debug_info: &DebugInfo,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        // The `byte_size` is used for arrays, etc. to offset the memory location of the next element.
        // For nested arrays, the `byte_size` may need to be calculated as the product of the `byte_size` and array upper bound.
        child_variable.byte_size = child_variable
            .byte_size
            .or_else(|| extract_byte_size(node_die))
            .or_else(|| {
                if let VariableType::Array { count, .. } = parent_variable.type_name {
                    parent_variable
                        .byte_size
                        .map(|byte_size| byte_size.checked_div(count as u64).unwrap_or(byte_size))
                } else {
                    None
                }
            });

        if child_variable.memory_location == VariableLocation::Unknown {
            // Any expected errors should be handled by one of the variants in the Ok() result.
            let expression_result = match self.extract_location(
                debug_info,
                node_die,
                &parent_variable.memory_location,
                child_variable.byte_size,
                memory,
                frame_info,
            ) {
                Ok(expr) => expr,
                Err(debug_error) => {
                    // An Err() result indicates something happened that we have not accounted for. Currently, we support all known location expressions for non-optimized code.
                    child_variable.memory_location = VariableLocation::Error(
                        "Unsupported location expression while resolving the location. Please reduce optimization levels in your build profile.".to_string()
                    );
                    let variable_name = &child_variable.name;
                    tracing::debug!(
                        "Encountered an unsupported location expression while resolving the location for variable {variable_name:?}: {debug_error:?}. Please reduce optimization levels in your build profile."
                    );
                    return Ok(());
                }
            };

            match expression_result {
                ExpressionResult::Location(VariableLocation::Unavailable) => {
                    child_variable.set_value(VariableValue::Error(
                        "<value optimized away by compiler, out of scope, or dropped>".to_string(),
                    ));
                }
                ExpressionResult::Location(
                    ref location @ VariableLocation::Error(ref error_message)
                    | ref location @ VariableLocation::Unsupported(ref error_message),
                ) => {
                    child_variable.set_value(VariableValue::Error(error_message.clone()));
                    child_variable.memory_location = location.clone();
                }
                ExpressionResult::Location(location_from_expression) => {
                    child_variable.memory_location = location_from_expression;
                }
            }
        }

        self.handle_memory_location_special_cases(
            node_die.offset(),
            child_variable,
            parent_variable,
            memory,
        );

        Ok(())
    }

    /// - Find the location using either DW_AT_location, DW_AT_data_member_location, or DW_AT_frame_base attribute.
    ///
    /// Return values are implemented as follows:
    /// - `Result<_, DebugError>`: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. **NOT GRACEFUL**, and should be avoided.
    /// - `Result<ExpressionResult::Location(),_>`: One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn extract_location(
        &self,
        debug_info: &DebugInfo,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_location: &VariableLocation,
        byte_size: Option<u64>,
        memory: &mut dyn MemoryInterface,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<ExpressionResult, DebugError> {
        trait ResultExt {
            /// Turns UnwindIncompleteResults into Unavailable locations
            fn convert_incomplete(self) -> Result<ExpressionResult, DebugError>;
        }

        impl ResultExt for Result<ExpressionResult, DebugError> {
            fn convert_incomplete(self) -> Result<ExpressionResult, DebugError> {
                match self {
                    Ok(result) => Ok(result),
                    Err(DebugError::WarnAndContinue { message }) => {
                        tracing::warn!("UnwindIncompleteResults: {:?}", message);
                        Ok(ExpressionResult::Location(VariableLocation::Unavailable))
                    }
                    e => e,
                }
            }
        }

        for attr in node_die.attrs() {
            let result = match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_frame_base
                | gimli::DW_AT_data_member_location => match attr.value() {
                    AttributeValue::Exprloc(expression) => self
                        .evaluate_expression(debug_info, memory, expression, frame_info)
                        .convert_incomplete()?,

                    AttributeValue::Udata(offset_from_location) => ExpressionResult::Location(
                        parent_location.offset_by(offset_from_location, byte_size),
                    ),

                    other_attribute_value => {
                        match debug_info
                            .dwarf
                            .attr_locations_offset(&self.unit, other_attribute_value.clone())
                        {
                            Ok(Some(location_list_offset)) => self
                                .evaluate_location_list_ref(
                                    debug_info,
                                    location_list_offset,
                                    frame_info,
                                    memory,
                                )
                                .convert_incomplete()?,
                            Ok(None) => {
                                ExpressionResult::Location(VariableLocation::Unsupported(format!(
                                    "Unimplemented: extract_location() Could not extract location from: {:.100}",
                                    format!("{other_attribute_value:?}")
                                )))
                            }
                            Err(error) => ExpressionResult::Location(VariableLocation::Error(
                                format!("Error: Resolving variable Location: {error:?}"),
                            )),
                        }
                    }
                },

                gimli::DW_AT_address_class => {
                    let location = match attr.value() {
                        AttributeValue::AddressClass(gimli::DwAddr(0)) => {
                            // We pass on the location of the parent, which will later to be used along with DW_AT_data_member_location to calculate the location of this variable.
                            parent_location.clone()
                        }
                        AttributeValue::AddressClass(address_class) => {
                            VariableLocation::Unsupported(format!(
                                "Unimplemented: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({address_class:?}))"
                            ))
                        }
                        other_attribute_value => VariableLocation::Unsupported(format!(
                            "Unimplemented: extract_location() found invalid DW_AT_address_class: {:.100}",
                            format!("{other_attribute_value:?}")
                        )),
                    };

                    ExpressionResult::Location(location)
                }

                _other_attributes => {
                    // These will be handled elsewhere.
                    continue;
                }
            };

            return Ok(result);
        }

        // If we get here, we did not find a location attribute, then leave the value as Unknown.
        Ok(ExpressionResult::Location(VariableLocation::Unknown))
    }

    fn evaluate_location_list_ref(
        &self,
        debug_info: &DebugInfo,
        location_list_offset: gimli::LocationListsOffset,
        frame_info: &StackFrameInfo<'_>,
        memory: &mut dyn MemoryInterface,
    ) -> Result<ExpressionResult, DebugError> {
        let mut locations = match debug_info.dwarf.locations(&self.unit, location_list_offset) {
            Ok(locations) => locations,
            Err(error) => {
                return Ok(ExpressionResult::Location(VariableLocation::Error(
                    format!("Error: Resolving variable Location: {error:?}"),
                )));
            }
        };
        let Some(program_counter) = frame_info
            .registers
            .get_program_counter()
            .and_then(|reg| reg.value)
        else {
            return Ok(ExpressionResult::Location(VariableLocation::Error(
                "Cannot determine variable location without a valid program counter.".to_string(),
            )));
        };

        let mut expression = None;
        'find_range: loop {
            let location = match locations.next() {
                Ok(Some(location_lists_entry)) => location_lists_entry,
                Ok(None) => break 'find_range,
                Err(error) => {
                    return Ok(ExpressionResult::Location(VariableLocation::Error(
                        format!("Error while iterating LocationLists for this variable: {error:?}"),
                    )));
                }
            };

            if let Ok(program_counter) = program_counter.try_into()
                && location.range.contains(program_counter)
            {
                expression = Some(location.data);
                break 'find_range;
            }
        }

        let Some(valid_expression) = expression else {
            return Ok(ExpressionResult::Location(VariableLocation::Unavailable));
        };

        self.evaluate_expression(debug_info, memory, valid_expression, frame_info)
    }

    /// Evaluate a [`gimli::Expression`] as a valid memory location.
    /// Return values are implemented as follows:
    /// - `Result<_, DebugError>`: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - `Result<ExpressionResult::Location(),_>`: One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn evaluate_expression(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<ExpressionResult, DebugError> {
        fn evaluate_address(address: u64, memory: &mut dyn MemoryInterface) -> ExpressionResult {
            let location = if address >= u32::MAX as u64 && !memory.supports_native_64bit_access() {
                VariableLocation::Error(format!(
                    "The memory location for this variable value ({address:#010X}) is invalid. Please report this as a bug."
                ))
            } else {
                VariableLocation::Address(address)
            };

            ExpressionResult::Location(location)
        }

        let pieces = self.expression_to_piece(debug_info, memory, expression, frame_info, 0)?;

        let [piece] = &pieces[..] else {
            if pieces.is_empty() {
                return Ok(ExpressionResult::Location(VariableLocation::Error(
                    "Error: expr_to_piece() returned 0 results".to_string(),
                )));
            }

            return self.assemble_pieces(&pieces, frame_info);
        };

        // A piece that does not start at a byte boundary, or that holds a part of a byte, needs
        // the bits of the pieces to be assembled.
        let bit_offset = piece.bit_offset.unwrap_or(0);
        let aligned = bit_offset % 8 == 0 && piece.size_in_bits.is_none_or(|bits| bits % 8 == 0);

        let result = match &piece.location {
            Location::Empty => {
                // This means the value was optimized away.
                ExpressionResult::Location(VariableLocation::Unavailable)
            }
            Location::Address { address: 0 } => {
                let error = "The value of this variable may have been optimized out of the debug info, by the compiler.".to_string();
                ExpressionResult::Location(VariableLocation::Error(error))
            }
            Location::Address { address } if aligned => {
                evaluate_address(address + bit_offset / 8, memory)
            }
            Location::Register { register } if aligned && bit_offset == 0 => {
                if let Some(value) = frame_info
                    .registers
                    .get_register_by_dwarf_id(register.0)
                    .and_then(|register| register.value)
                {
                    ExpressionResult::Location(VariableLocation::RegisterValue(value))
                } else {
                    ExpressionResult::Location(VariableLocation::Error(format!(
                        "Error: Cannot resolve register: {register:?}"
                    )))
                }
            }
            _partial_piece => return self.assemble_pieces(&pieces, frame_info),
        };

        Ok(result)
    }

    /// Describe a value that is assembled from more than one place, or from a part of a place.
    fn assemble_pieces(
        &self,
        pieces: &[gimli::Piece<GimliReader, usize>],
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<ExpressionResult, DebugError> {
        let mut location_pieces = Vec::with_capacity(pieces.len());

        for piece in pieces {
            let source = match &piece.location {
                Location::Empty => PieceSource::Empty,
                Location::Address { address } => PieceSource::Address(*address),
                Location::Register { register } => {
                    let Some(value) = frame_info
                        .registers
                        .get_register_by_dwarf_id(register.0)
                        .and_then(|register| register.value)
                    else {
                        return Ok(ExpressionResult::Location(VariableLocation::Error(
                            format!("Error: Cannot resolve register: {register:?}"),
                        )));
                    };

                    PieceSource::Register(value)
                }
                Location::Value { value } => PieceSource::Implicit(value_bytes(*value)),
                Location::Bytes { value } => {
                    PieceSource::Implicit(gimli::Reader::to_slice(value)?.to_vec())
                }
                location @ Location::ImplicitPointer { .. } => {
                    return Ok(ExpressionResult::Location(VariableLocation::Unsupported(
                        format!(
                            "Unimplemented: extract_location() found a location type: {:.100}",
                            format!("{location:?}")
                        ),
                    )));
                }
            };

            location_pieces.push(LocationPiece {
                source,
                bit_offset: piece.bit_offset.unwrap_or(0),
                bit_size: piece.size_in_bits,
            });
        }

        Ok(ExpressionResult::Location(VariableLocation::Composite(
            location_pieces,
        )))
    }

    /// Tries to get the result of a DWARF expression in the form of a Piece.
    pub(crate) fn expression_to_piece(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        frame_info: &StackFrameInfo<'_>,
        entry_value_depth: u8,
    ) -> Result<Vec<gimli::Piece<GimliReader, usize>>, DebugError> {
        let mut evaluation = expression.evaluation(self.unit.encoding());
        let mut result = evaluation.evaluate()?;

        loop {
            result = match result {
                EvaluationResult::Complete => return Ok(evaluation.result()),
                EvaluationResult::RequiresMemory { address, size, .. } => {
                    read_memory(size, memory, address, &mut evaluation)?
                }
                EvaluationResult::RequiresFrameBase => {
                    provide_frame_base(frame_info.frame_base, &mut evaluation)?
                }
                EvaluationResult::RequiresRegister {
                    register,
                    base_type,
                } => provide_register(frame_info.registers, register, base_type, &mut evaluation)?,
                EvaluationResult::RequiresRelocatedAddress(address_index) => {
                    // The address_index as an offset from 0, so just pass it into the next step.
                    evaluation.resume_with_relocated_address(address_index)?
                }
                EvaluationResult::RequiresCallFrameCfa => {
                    provide_cfa(frame_info.canonical_frame_address, &mut evaluation)?
                }
                EvaluationResult::RequiresEntryValue(inner) => {
                    let value = self.resolve_entry_value(
                        debug_info,
                        memory,
                        inner,
                        frame_info,
                        entry_value_depth,
                    )?;
                    evaluation.resume_with_entry_value(value)?
                }
                unimplemented_expression => {
                    return Err(DebugError::WarnAndContinue {
                        message: unsupported_evaluation_result(&unimplemented_expression),
                    });
                }
            }
        }
    }

    fn resolve_entry_value(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        inner: gimli::Expression<GimliReader>,
        frame_info: &StackFrameInfo<'_>,
        entry_value_depth: u8,
    ) -> Result<gimli::Value, DebugError> {
        if entry_value_depth >= 4 {
            return Err(DebugError::WarnAndContinue {
                message: "Nested DW_OP_entry_value is not supported.".to_string(),
            });
        }

        let Some(caller) = frame_info.caller else {
            return Err(DebugError::WarnAndContinue {
                message: "DW_OP_entry_value requires a caller frame.".to_string(),
            });
        };

        let expression = caller
            .program_counter()
            .and_then(|pc| debug_info.call_site_value(pc, &inner));

        if let Some((unit, expression)) = expression {
            unit.expression_to_value(
                debug_info,
                memory,
                expression,
                caller,
                entry_value_depth + 1,
            )
        } else {
            self.expression_to_value(debug_info, memory, inner, caller, entry_value_depth + 1)
        }
    }

    fn expression_to_value(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        frame_info: &StackFrameInfo<'_>,
        entry_value_depth: u8,
    ) -> Result<gimli::Value, DebugError> {
        let mut evaluation = expression.evaluation(self.unit.encoding());
        let mut result = evaluation.evaluate()?;

        loop {
            result = match result {
                EvaluationResult::Complete => {
                    if let Some(value) = evaluation.value_result() {
                        return Ok(value);
                    }
                    return pieces_to_value(&evaluation.result(), frame_info);
                }
                EvaluationResult::RequiresMemory { address, size, .. } => {
                    read_memory(size, memory, address, &mut evaluation)?
                }
                EvaluationResult::RequiresFrameBase => {
                    provide_frame_base(frame_info.frame_base, &mut evaluation)?
                }
                EvaluationResult::RequiresRegister {
                    register,
                    base_type,
                } => provide_register(frame_info.registers, register, base_type, &mut evaluation)?,
                EvaluationResult::RequiresRelocatedAddress(address_index) => {
                    evaluation.resume_with_relocated_address(address_index)?
                }
                EvaluationResult::RequiresCallFrameCfa => {
                    provide_cfa(frame_info.canonical_frame_address, &mut evaluation)?
                }
                EvaluationResult::RequiresEntryValue(inner) => {
                    let value = self.resolve_entry_value(
                        debug_info,
                        memory,
                        inner,
                        frame_info,
                        entry_value_depth,
                    )?;
                    evaluation.resume_with_entry_value(value)?
                }
                unimplemented_expression => {
                    return Err(DebugError::WarnAndContinue {
                        message: unsupported_evaluation_result(&unimplemented_expression),
                    });
                }
            }
        }
    }

    /// A helper function, to handle memory_location for special cases, such as array members, pointers, and intermediate nodes.
    /// Normally, the memory_location is calculated before the type is calculated,
    ///     but special cases require the type related info of the variable to correctly compute the memory_location.
    fn handle_memory_location_special_cases(
        &self,
        unit_ref: UnitOffset,
        child_variable: &mut Variable,
        parent_variable: &Variable,
        memory: &mut dyn MemoryInterface,
    ) {
        let location = if let VariableName::Indexed(child_member_index) = child_variable.name {
            // Push the array member to the proper location according to its index.
            if matches!(
                parent_variable.memory_location,
                VariableLocation::Address(_)
                    | VariableLocation::RegisterValue(_)
                    | VariableLocation::Composite(_)
            ) {
                if let Some(byte_size) = child_variable.byte_size {
                    parent_variable
                        .memory_location
                        .offset_by(child_member_index * byte_size, Some(byte_size))
                } else {
                    // If this array member doesn't have a byte_size, it may be because it is the first member of an array itself.
                    // In this case, the byte_size will be calculated when the nested array members are resolved.
                    // The first member of an array will have a memory location of the same as it's parent.
                    parent_variable.memory_location.clone()
                }
            } else {
                VariableLocation::Unavailable
            }
        } else if self.is_pointer(child_variable, parent_variable, unit_ref) {
            match &parent_variable.memory_location {
                VariableLocation::Address(_)
                | VariableLocation::RegisterValue(_)
                | VariableLocation::Composite(_)
                | VariableLocation::Value => match self.pointer_address(parent_variable, memory) {
                    Ok(address) => {
                        let alignment = self
                            .unit
                            .entry(unit_ref)
                            .ok()
                            .and_then(|entry| extract_alignment(&entry));

                        match object_at(address, alignment, child_variable.byte_size) {
                            Some(address) => VariableLocation::Address(address),
                            None if address == 0 => {
                                VariableLocation::Error("<null pointer>".to_string())
                            }
                            None => VariableLocation::Error(format!(
                                "<dangling pointer: {address:#010X}>"
                            )),
                        }
                    }
                    Err(error) => {
                        tracing::debug!(
                            "Failed to read referenced variable address from memory location {} : {error}.",
                            parent_variable.memory_location
                        );
                        VariableLocation::Error(format!(
                            "Failed to read referenced variable address from memory location {} : {error}.",
                            parent_variable.memory_location
                        ))
                    }
                },
                other => VariableLocation::Unsupported(format!(
                    "Location {other:?} not supported for referenced variables."
                )),
            }
        } else if child_variable.memory_location == VariableLocation::Unknown {
            // A variable that is not a referenced value shares the location of its parent, for
            // example an intermediate node of a struct.
            parent_variable.memory_location.clone()
        } else {
            return;
        };

        child_variable.memory_location = location;
    }

    /// Whether following `pointer` yields an object that the debugger can read.
    ///
    /// A zero sized type has no object. A null pointer or a dangling pointer does not point at an
    /// object.
    pub(crate) fn points_at_an_object(
        &self,
        debug_info: &DebugInfo,
        pointer: &Variable,
        memory: &mut dyn MemoryInterface,
        pointee_unit: &UnitInfo,
        pointee_offset: UnitOffset,
    ) -> bool {
        let (byte_size, alignment) = pointee_unit.type_layout(debug_info, pointee_offset);
        match self.pointer_address(pointer, memory) {
            Ok(address) => object_at(address, alignment, byte_size).is_some(),
            Err(_) => false,
        }
    }

    /// The `DW_AT_byte_size` and `DW_AT_alignment` of the type at `offset`, following type
    /// modifiers that carry none of their own.
    fn type_layout(
        &self,
        debug_info: &DebugInfo,
        mut offset: UnitOffset,
    ) -> (Option<u64>, Option<u64>) {
        let mut unit = self;
        for _ in 0..16 {
            let Ok(entry) = unit.unit.entry(offset) else {
                return (None, None);
            };
            let byte_size = extract_byte_size(&entry);
            let alignment = extract_alignment(&entry);
            if byte_size.is_some() || !is_type_modifier(entry.tag()) {
                return (byte_size, alignment);
            }
            let Some(attr) = entry.attr(gimli::DW_AT_type) else {
                return (byte_size, alignment);
            };
            match debug_info.resolve_die_reference_with_unit(attr, unit) {
                Ok((next_unit, next_entry)) => {
                    unit = next_unit;
                    offset = next_entry.offset();
                }
                Err(_) => return (byte_size, alignment),
            }
        }
        (None, None)
    }

    /// The address that a pointer holds.
    fn pointer_address(
        &self,
        pointer: &Variable,
        memory: &mut dyn MemoryInterface,
    ) -> Result<u64, DebugError> {
        let mut buffer = [0u8; 8];
        let address_size = (self.unit.encoding().address_size as usize).min(8);

        match &pointer.memory_location {
            VariableLocation::Address(_)
            | VariableLocation::RegisterValue(_)
            | VariableLocation::Composite(_) => {
                pointer
                    .memory_location
                    .read(&mut buffer[..address_size], memory)?;
                Ok(u64::from_le_bytes(buffer))
            }
            VariableLocation::Value => match &pointer.value {
                VariableValue::Valid(value) => {
                    value.parse().map_err(|_| DebugError::WarnAndContinue {
                        message: format!("The pointer value `{value}` is not an address"),
                    })
                }
                other => Err(DebugError::WarnAndContinue {
                    message: format!("The pointer has no address: {other}"),
                }),
            },
            other => Err(DebugError::WarnAndContinue {
                message: format!("Location {other:?} not supported for referenced variables."),
            }),
        }
    }

    /// Returns `true` if the variable is a pointer, `false` otherwise.
    fn is_pointer(
        &self,
        child_variable: &mut Variable,
        parent_variable: &Variable,
        unit_ref: UnitOffset,
    ) -> bool {
        // Address Pointer Conditions (any of):
        // 1. Variable names that start with '*' (e.g '*__0), AND the variable is a variant of the parent.
        // 2. Pointer names that start with '*' (e.g. '*const u8')
        // 3. Pointers to base types (includes &str types)
        // 4. Pointers to variable names that start with `*`
        // 5. Pointers to types with referenced memory addresses (e.g. variants, generics, arrays, etc.)
        (matches!(child_variable.name, VariableName::Named(ref var_name) if var_name.starts_with('*'))
                && matches!(parent_variable.role, VariantRole::VariantPart(_)))
            || parent_variable
                .type_name
                .ident()
                .is_some_and(|name| name.starts_with('*'))
            || (matches!(&parent_variable.type_name, VariableType::Pointer(_))
                && (matches!(child_variable.type_name, VariableType::Base(_))
                    || matches!(child_variable.type_name, VariableType::Struct(ref type_name) if type_name.ident_stem().starts_with("&str"))
                    || matches!(child_variable.name, VariableName::Named(ref var_name) if var_name.starts_with('*'))
                    || self.has_address_pointer(unit_ref).unwrap_or_else(|error| {
                        child_variable.set_value(VariableValue::Error(format!("Failed to determine if a struct has variant or generic type fields: {error}")));
                        false
                    })))
    }

    /// A helper function to determine if the type we are referencing requires a pointer to the address of the referenced variable (e.g. variants, generics, arrays, etc.)
    fn has_address_pointer(&self, unit_ref: UnitOffset) -> Result<bool, DebugError> {
        let mut entries_tree = self.unit.entries_tree(Some(unit_ref))?;
        let entry_node = entries_tree.root()?;
        if matches!(
            entry_node.entry().tag(),
            gimli::DW_TAG_array_type | gimli::DW_TAG_enumeration_type | gimli::DW_TAG_union_type
        ) {
            return Ok(true);
        }
        // If the child node has a variant_part, then the variant will be a pointer to the address of the referenced variable.
        let mut child_nodes = entry_node.children();
        while let Some(child_node) = child_nodes.next()? {
            if child_node.entry().tag() == gimli::DW_TAG_variant_part {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn extract_named_type(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        raw_name: String,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> NamedType {
        let namespace = self.namespace_path(debug_info, node);
        let args = self.extract_generic_args(debug_info, node, visiting);
        NamedType::from_dwarf(raw_name, namespace, args, self.language.as_ref())
    }

    fn parse_or_base(&self, name: String) -> VariableType {
        self.language
            .parse_type_name(&name)
            .unwrap_or(VariableType::Base(name))
    }

    fn pointer_from_name(&self, name: Option<String>) -> VariableType {
        VariableType::Pointer(name.map(|name| {
            Box::new(
                self.language
                    .parse_type_name(&name)
                    .unwrap_or(VariableType::Other(name)),
            )
        }))
    }

    fn extract_generic_args(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> Vec<GenericArg> {
        let Ok(mut tree) = self.unit.entries_tree(Some(node.offset())) else {
            return Vec::new();
        };
        let Ok(root) = tree.root() else {
            return Vec::new();
        };
        let mut args = Vec::new();
        self.collect_generic_args(debug_info, root, visiting, &mut args);
        args
    }

    fn collect_generic_args(
        &self,
        debug_info: &DebugInfo,
        node: gimli::EntriesTreeNode<'_, '_, GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
        args: &mut Vec<GenericArg>,
    ) {
        let mut children = node.children();
        while let Ok(Some(child)) = children.next() {
            match child.entry().tag() {
                gimli::DW_TAG_template_type_parameter => {
                    if let Some(arg) = self.template_type_arg(debug_info, child.entry(), visiting) {
                        args.push(arg);
                    }
                }
                gimli::DW_TAG_template_value_parameter => {
                    args.push(GenericArg::Const(self.template_const_arg(child.entry())));
                }
                gimli::DW_TAG_GNU_template_parameter_pack => {
                    self.collect_generic_args(debug_info, child, visiting, args);
                }
                _ => {}
            }
        }
    }

    fn template_type_arg(
        &self,
        debug_info: &DebugInfo,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> Option<GenericArg> {
        let attr = entry.attr(gimli::DW_AT_type)?;
        let (unit, ty_node) = debug_info
            .resolve_die_reference_with_unit(attr, self)
            .ok()?;
        Some(GenericArg::Type(
            unit.extract_variable_type(debug_info, &ty_node, visiting),
        ))
    }

    fn template_const_arg(&self, entry: &gimli::DebuggingInformationEntry<GimliReader>) -> String {
        let Some(attr) = entry.attr(gimli::DW_AT_const_value) else {
            return "<const>".to_string();
        };
        let value = attr.value();
        if let Some(const_value) = value.udata_value() {
            const_value.to_string()
        } else if let Some(const_value) = value.sdata_value() {
            const_value.to_string()
        } else {
            "<const>".to_string()
        }
    }

    fn extract_variable_type(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> VariableType {
        let Some(offset) = node.offset().to_debug_info_offset(&self.unit.header) else {
            return VariableType::Unknown;
        };
        if !visiting.insert(offset) {
            return self.cycle_break_type(debug_info, node);
        }
        let ty = self.extract_variable_type_inner(debug_info, node, visiting);
        visiting.remove(&offset);
        ty
    }

    fn cycle_break_type(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
    ) -> VariableType {
        let name = self
            .extract_type_name(debug_info, node)
            .ok()
            .flatten()
            .unwrap_or_else(|| "<recursive>".to_string());
        let named = NamedType::from_dwarf(
            name,
            self.namespace_path(debug_info, node),
            Vec::new(),
            self.language.as_ref(),
        );
        match node.tag() {
            gimli::DW_TAG_structure_type => VariableType::Struct(named),
            gimli::DW_TAG_enumeration_type => VariableType::Enum(named),
            gimli::DW_TAG_base_type => VariableType::Base(named.ident.to_string()),
            gimli::DW_TAG_pointer_type => self.pointer_from_name(Some(named.ident.to_string())),
            _ => VariableType::Other(named.ident.to_string()),
        }
    }

    fn extract_variable_type_inner(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> VariableType {
        let name = self.extract_type_name(debug_info, node).ok().flatten();

        match node.tag() {
            gimli::DW_TAG_base_type => {
                self.parse_or_base(name.unwrap_or_else(|| "<unnamed base type>".to_string()))
            }
            gimli::DW_TAG_pointer_type => self.pointer_from_name(name),
            gimli::DW_TAG_structure_type => VariableType::Struct(self.extract_named_type(
                debug_info,
                node,
                name.unwrap_or_else(|| "<unnamed struct>".to_string()),
                visiting,
            )),
            gimli::DW_TAG_enumeration_type => VariableType::Enum(self.extract_named_type(
                debug_info,
                node,
                name.unwrap_or_else(|| "<unnamed enum>".to_string()),
                visiting,
            )),
            gimli::DW_TAG_union_type => {
                VariableType::Base(name.unwrap_or_else(|| "<unnamed union>".to_string()))
            }
            gimli::DW_TAG_array_type => {
                self.extract_array_variable_type(debug_info, node, visiting)
            }
            other @ (gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type) => {
                let inner = match node.attr(gimli::DW_AT_type) {
                    Some(attr) => debug_info
                        .resolve_die_reference_with_unit(attr, self)
                        .map(|(unit, ty_node)| {
                            unit.extract_variable_type(debug_info, &ty_node, visiting)
                        })
                        .unwrap_or(VariableType::Unknown),
                    None => VariableType::Unknown,
                };
                let modifier = match other {
                    gimli::DW_TAG_typedef => {
                        Modifier::Typedef(name.unwrap_or_else(|| "<unnamed typedef>".to_string()))
                    }
                    gimli::DW_TAG_const_type => Modifier::Const,
                    gimli::DW_TAG_volatile_type => Modifier::Volatile,
                    gimli::DW_TAG_restrict_type => Modifier::Restrict,
                    gimli::DW_TAG_atomic_type => Modifier::Atomic,
                    _ => unreachable!(),
                };
                VariableType::Modified(modifier, Box::new(inner))
            }
            _ => VariableType::Other(name.unwrap_or_else(|| "unimplemented".to_string())),
        }
    }

    fn extract_array_variable_type(
        &self,
        debug_info: &DebugInfo,
        node: &gimli::DebuggingInformationEntry<GimliReader>,
        visiting: &mut HashSet<DebugInfoOffset>,
    ) -> VariableType {
        let count = self
            .extract_array_range(node.offset())
            .ok()
            .and_then(|ranges| ranges.into_iter().next())
            .map(|range| range.count())
            .unwrap_or(0);

        let item_type_name = match node.attr(gimli::DW_AT_type) {
            Some(attr) => debug_info
                .resolve_die_reference_with_unit(attr, self)
                .map(|(unit, item_node)| {
                    unit.extract_variable_type(debug_info, &item_node, visiting)
                })
                .unwrap_or(VariableType::Unknown),
            None => VariableType::Unknown,
        };

        VariableType::Array {
            item_type_name: Box::new(item_type_name),
            count,
        }
    }

    pub(crate) fn extract_type_name(
        &self,
        debug_info: &DebugInfo,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
    ) -> Result<Option<String>, gimli::Error> {
        match entry.attr(gimli::DW_AT_name) {
            Some(attr) => Ok(Some(attribute_string(debug_info, &self.unit, attr.value()))),
            None => {
                let Some(attr) = entry.attr(gimli::DW_AT_type) else {
                    // No type attribute.
                    return Ok(None);
                };

                // Try to read the name of the referenced type node.
                let Ok((referenced_unit, node)) =
                    debug_info.resolve_die_reference_with_unit(attr, self)
                else {
                    return Ok(None);
                };

                referenced_unit.extract_type_name(debug_info, &node)
            }
        }
    }

    fn process_bitfield_info(
        &self,
        child_variable: &mut Variable,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
        cache: &mut VariableCache,
    ) -> Result<(), DebugError> {
        if !child_variable.is_valid() {
            // Only bother with bitfields if we haven't encountered an error yet
            return Ok(());
        }
        match self.extract_bitfield_info(child_variable, entry) {
            Ok(Some(bitfield)) => {
                if let Some(byte_size) = child_variable.byte_size {
                    let bitfield = bitfield.normalize(byte_size);
                    child_variable.type_name = VariableType::Bitfield(
                        bitfield,
                        Box::new(std::mem::replace(
                            &mut child_variable.type_name,
                            VariableType::Unknown,
                        )),
                    );
                    // Invalidate value that was read before we knew about the bitfield.
                    child_variable.value = VariableValue::Empty;
                    cache.update_variable(child_variable)?;
                } else {
                    child_variable.set_value(VariableValue::Error(
                        "Error: Failed to decode bitfield information: byte_size not found"
                            .to_string(),
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => child_variable.set_value(VariableValue::Error(format!(
                "Error: Failed to decode bitfield information: {e:?}"
            ))),
        }

        Ok(())
    }

    fn extract_bitfield_info(
        &self,
        child_variable: &mut Variable,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
    ) -> Result<Option<Bitfield>, gimli::Error> {
        let offset = if let Some(attr) = entry.attr(gimli::DW_AT_data_bit_offset) {
            // Available since DWARF 4+
            match attr.value().udata_value() {
                Some(offset) => Some(BitOffset::FromLsb(offset)),
                None => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "Unimplemented: Attribute Value for DW_AT_data_bit_offset: {:?}",
                        attr.value()
                    )));
                    return Ok(None);
                }
            }
        } else if let Some(attr) = entry.attr(gimli::DW_AT_bit_offset) {
            // Deprecated in DWARF 5, but still used by some compilers.
            // Specifies offset from MSB. We're handling this as a separate offset variant
            // because we haven't yet processed the byte size of the variable.
            if let Some(offset) = attr.value().udata_value() {
                Some(BitOffset::FromMsb(offset))
            } else {
                child_variable.set_value(VariableValue::Error(format!(
                    "Unimplemented: Attribute Value for DW_AT_bit_offset: {:?}",
                    attr.value()
                )));
                return Ok(None);
            }
        } else {
            None
        };

        let size = if let Some(attr) = entry.attr(gimli::DW_AT_bit_size) {
            match attr.value().udata_value() {
                Some(length) => Some(length),
                None => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "Unimplemented: Attribute Value for DW_AT_bit_size: {:?}",
                        attr.value()
                    )));
                    return Ok(None);
                }
            }
        } else {
            None
        };

        // Without a bit size this is not a bitfield, but a member at a byte offset.
        let Some(length) = size else {
            return Ok(None);
        };

        Ok(Some(Bitfield {
            length,
            offset: offset.unwrap_or(BitOffset::FromLsb(0)),
        }))
    }

    fn extract_source_location(
        &self,
        debug_info: &DebugInfo,
        entry: &gimli::DebuggingInformationEntry<GimliReader>,
    ) -> Result<Option<SourceLocation>, gimli::Error> {
        let Some(file_attr) = entry.attr_value(gimli::DW_AT_decl_file) else {
            return Ok(None);
        };

        let Some(path) = extract_file(debug_info, &self.unit, file_attr) else {
            return Ok(None);
        };

        let mut source_location = SourceLocation {
            path,
            line: None,
            column: None,
            address: None,
        };

        // Now loop through all the unit attributes to extract the remainder of the `Variable` definition.
        for attr in entry.attrs() {
            match attr.name() {
                gimli::DW_AT_decl_line => {
                    if let Some(line_number) = extract_line(attr.value()) {
                        source_location.line = Some(line_number);
                    }
                }
                gimli::DW_AT_decl_column => {
                    if let Some(column_number) = attr.udata_value() {
                        // According to the DWARF standard, a value of 0 means no column is specified.
                        if column_number != 0 {
                            source_location.column = Some(super::ColumnType::Column(column_number));
                        }
                    }
                }
                // Other attributes are not relevant for extracting source location.
                _ => (),
            }
        }

        Ok(Some(source_location))
    }

    pub(crate) fn parent_offset(&self, offset: UnitOffset) -> Option<UnitOffset> {
        self.parents.get(&offset).copied()
    }

    /// Names of enclosing namespace, module, function, and type DIEs, crate first.
    pub(crate) fn enclosing_path(
        &self,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Vec<String> {
        let mut segments = Vec::new();
        let mut offset = self.parent_offset(entry.offset());
        while let Some(parent) = offset {
            if let Ok(die) = self.unit.entry(parent)
                && is_enclosing_name_tag(die.tag())
                && let Ok(Some(name)) = extract_name(debug_info, &self.unit, &die)
            {
                segments.push(name);
            }

            offset = self.parent_offset(parent);
        }
        segments.reverse();
        segments
    }

    /// Names of enclosing `DW_TAG_namespace` / `DW_TAG_module` DIEs, crate first.
    pub(crate) fn namespace_path(
        &self,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Vec<String> {
        let mut segments = Vec::new();
        let mut offset = self.parent_offset(entry.offset());
        while let Some(parent) = offset {
            if let Ok(die) = self.unit.entry(parent)
                && matches!(die.tag(), gimli::DW_TAG_namespace | gimli::DW_TAG_module)
                && let Ok(Some(name)) = extract_name(debug_info, &self.unit, &die)
            {
                segments.push(name);
            }

            offset = self.parent_offset(parent);
        }
        segments.reverse();
        segments
    }
}

fn is_enclosing_name_tag(tag: gimli::DwTag) -> bool {
    matches!(
        tag,
        gimli::DW_TAG_namespace
            | gimli::DW_TAG_module
            | gimli::DW_TAG_subprogram
            | gimli::DW_TAG_inlined_subroutine
            | gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_union_type
            | gimli::DW_TAG_enumeration_type
    )
}

fn extract_name(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<GimliReader>,
    entry: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Result<Option<String>, gimli::Error> {
    let Some(attr) = entry.attr_value(gimli::DW_AT_name) else {
        return Ok(None);
    };

    Ok(Some(attribute_string(debug_info, unit, attr)))
}

/// Reads a string attribute, whatever form the compiler used to encode it.
fn attribute_string(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<GimliReader>,
    attr: AttributeValue<GimliReader>,
) -> String {
    match debug_info.dwarf.attr_string(unit, attr) {
        Ok(raw) => String::from_utf8_lossy(&raw).to_string(),
        Err(error) => format!("Invalid string attribute value: {error}"),
    }
}

struct CallSiteParameter {
    location: Vec<u8>,
    value: gimli::Expression<GimliReader>,
}

fn collect_call_site(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
    active_call_site: &mut Option<(isize, u64)>,
    call_sites: &mut HashMap<u64, Vec<CallSiteParameter>>,
) {
    let tag = die.tag();
    if tag == gimli::DW_TAG_call_site || tag == gimli::DW_TAG_GNU_call_site {
        *active_call_site = call_site_return_pc(die, dwarf, unit).map(|pc| (die.depth(), pc));
        return;
    }

    let Some((site_depth, pc)) = *active_call_site else {
        return;
    };

    if die.depth() <= site_depth {
        *active_call_site = None;
        return;
    }

    if die.depth() != site_depth + 1 {
        return;
    }

    if tag != gimli::DW_TAG_call_site_parameter && tag != gimli::DW_TAG_GNU_call_site_parameter {
        return;
    }

    let Some(param) = call_site_parameter(die) else {
        return;
    };
    call_sites.entry(pc).or_default().push(param);
}

fn call_site_return_pc(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
) -> Option<u64> {
    die_address(die, dwarf, unit, gimli::DW_AT_call_return_pc)
        .or_else(|| die_address(die, dwarf, unit, gimli::DW_AT_low_pc))
        .or_else(|| die_address(die, dwarf, unit, gimli::DW_AT_call_pc))
}

fn die_address(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    dwarf: &gimli::Dwarf<GimliReader>,
    unit: &gimli::Unit<GimliReader>,
    attr: gimli::DwAt,
) -> Option<u64> {
    match die.attr_value(attr)? {
        AttributeValue::Addr(address) => Some(address),
        AttributeValue::DebugAddrIndex(index) => dwarf.address(unit, index).ok(),
        AttributeValue::Udata(value) => Some(value),
        _ => None,
    }
}

fn call_site_parameter(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Option<CallSiteParameter> {
    let location = exprloc(die, gimli::DW_AT_location)?;
    let value = exprloc(die, gimli::DW_AT_call_value)
        .or_else(|| exprloc(die, gimli::DW_AT_GNU_call_site_value))?;
    Some(CallSiteParameter {
        location: expression_bytes(&location)?,
        value,
    })
}

fn exprloc(
    die: &gimli::DebuggingInformationEntry<GimliReader>,
    attr: gimli::DwAt,
) -> Option<gimli::Expression<GimliReader>> {
    match die.attr_value(attr)? {
        AttributeValue::Exprloc(expression) => Some(expression),
        _ => None,
    }
}

fn expression_bytes(expression: &gimli::Expression<GimliReader>) -> Option<Vec<u8>> {
    gimli::Reader::to_slice(&expression.0)
        .ok()
        .map(|bytes| bytes.to_vec())
}

fn pieces_to_value(
    pieces: &[gimli::Piece<GimliReader, usize>],
    frame_info: &StackFrameInfo<'_>,
) -> Result<gimli::Value, DebugError> {
    let [piece] = pieces else {
        return Err(DebugError::WarnAndContinue {
            message: "DW_OP_entry_value produced a composite location.".to_string(),
        });
    };

    match &piece.location {
        Location::Value { value } => Ok(*value),
        Location::Register { register } => {
            let Some(raw_value) = frame_info
                .registers
                .get_register_by_dwarf_id(register.0)
                .and_then(|register| register.value)
            else {
                return Err(DebugError::WarnAndContinue {
                    message: format!(
                        "DW_OP_entry_value has no value for register #:{}.",
                        register.0
                    ),
                });
            };
            Ok(gimli::Value::Generic(raw_value.try_into()?))
        }
        Location::Address { address } => Ok(gimli::Value::Generic(*address)),
        Location::Bytes { value } => {
            let bytes = gimli::Reader::to_slice(value)?;
            let mut buf = [0u8; 8];
            let len = bytes.len().min(8);
            buf[..len].copy_from_slice(&bytes[..len]);
            Ok(gimli::Value::Generic(u64::from_le_bytes(buf)))
        }
        Location::Empty => Err(DebugError::WarnAndContinue {
            message: "DW_OP_entry_value produced an empty location.".to_string(),
        }),
        Location::ImplicitPointer { .. } => Err(DebugError::WarnAndContinue {
            message: "DW_OP_entry_value produced an implicit pointer.".to_string(),
        }),
    }
}

fn unsupported_evaluation_result<R: gimli::Reader>(result: &EvaluationResult<R>) -> String {
    let kind = match result {
        EvaluationResult::RequiresTls(_) => "DW_OP_form_tls_address",
        EvaluationResult::RequiresAtLocation(_) => "DW_OP_call",
        EvaluationResult::RequiresParameterRef(_) => "DW_OP_parameter_ref",
        EvaluationResult::RequiresIndexedAddress { .. } => "an indexed address",
        EvaluationResult::RequiresBaseType(_) => "a typed DWARF value",
        EvaluationResult::RequiresEntryValue(_) => "DW_OP_entry_value",
        _ => "this DWARF evaluation request",
    };
    format!("Unimplemented: {kind} is not currently supported.")
}

/// Gets necessary register information for the DWARF resolver.
fn provide_register(
    stack_frame_registers: &DebugRegisters,
    register: gimli::Register,
    base_type: UnitOffset,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    match stack_frame_registers
        .get_register_by_dwarf_id(register.0)
        .and_then(|reg| reg.value)
    {
        Some(raw_value) if base_type == gimli::UnitOffset(0) => {
            let register_value = gimli::Value::Generic(raw_value.try_into()?);
            Ok(evaluation.resume_with_register(register_value)?)
        }
        Some(_) => Err(DebugError::WarnAndContinue {
            message: format!("Unimplemented: Support for type {base_type:?} in `RequiresRegister`"),
        }),
        None => Err(DebugError::WarnAndContinue {
            message: format!(
                "Error while calculating `Variable::memory_location`. No value for register #:{}.",
                register.0
            ),
        }),
    }
}

/// Gets necessary framebase information for the DWARF resolver.
fn provide_frame_base(
    frame_base: Option<u64>,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    let Some(frame_base) = frame_base else {
        return Err(DebugError::WarnAndContinue {
            message: "Cannot unwind `Variable` location without a valid frame base address.)"
                .to_string(),
        });
    };
    match evaluation.resume_with_frame_base(frame_base) {
        Ok(evaluation_result) => Ok(evaluation_result),
        Err(error) => Err(DebugError::WarnAndContinue {
            message: format!("Error while calculating `Variable::memory_location`:{error}."),
        }),
    }
}

/// Gets necessary CFA information for the DWARF resolver.
fn provide_cfa(
    cfa: Option<u64>,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    let Some(cfa) = cfa else {
        return Err(DebugError::WarnAndContinue {
            message: "Cannot unwind `Variable` location without a valid canonical frame address.)"
                .to_string(),
        });
    };
    match evaluation.resume_with_call_frame_cfa(cfa) {
        Ok(evaluation_result) => Ok(evaluation_result),
        Err(error) => Err(DebugError::WarnAndContinue {
            message: format!("Error while calculating `Variable::memory_location`:{error}."),
        }),
    }
}

fn is_type_modifier(tag: gimli::DwTag) -> bool {
    matches!(
        tag,
        gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type
            | gimli::DW_TAG_restrict_type
            | gimli::DW_TAG_atomic_type
    )
}

/// The largest alignment that a type on a target has, in bytes.
const MAX_ALIGNMENT: u64 = 16;

/// The address of the referenced value of a pointer that holds `address`, if the pointer points
/// at an object.
///
/// A pointer of an empty collection holds the alignment of the type, not the address of an
/// object. `core::ptr::NonNull::dangling` creates such a pointer. `alignment` is the alignment
/// of the type, and `byte_size` its size, as far as the debug info gives them. A zero sized type
/// has no object.
fn object_at(address: u64, alignment: Option<u64>, byte_size: Option<u64>) -> Option<u64> {
    if address == 0 || byte_size == Some(0) {
        return None;
    }

    if let Some(alignment) = alignment
        && alignment > 1
        && !address.is_multiple_of(alignment)
    {
        return None;
    }

    let dangling = match alignment {
        Some(alignment) => address == alignment,
        // Without the alignment of the type, take every address that an alignment can be: a
        // power of two that is neither greater than the type nor greater than the largest
        // alignment of a target type.
        None => {
            address.is_power_of_two()
                && address <= MAX_ALIGNMENT
                && byte_size.is_none_or(|byte_size| address <= byte_size)
        }
    };

    (!dangling).then_some(address)
}

/// Keeps the lowest `byte_size` bytes of `value`.
fn truncate(value: u128, byte_size: usize) -> u128 {
    let shift = 128 - byte_size * 8;
    (value << shift) >> shift
}

/// The bytes of a value that the debug info holds, in little endian order.
fn value_bytes(value: gimli::Value) -> Vec<u8> {
    match value {
        gimli::Value::Generic(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I8(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U8(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I16(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U16(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::I64(value) => value.to_le_bytes().to_vec(),
        gimli::Value::U64(value) => value.to_le_bytes().to_vec(),
        gimli::Value::F32(value) => value.to_le_bytes().to_vec(),
        gimli::Value::F64(value) => value.to_le_bytes().to_vec(),
    }
}

/// Interprets the lowest `byte_size` bytes of `value` as a signed number.
fn sign_extend(value: u128, byte_size: usize) -> i128 {
    let shift = 128 - byte_size * 8;
    ((value << shift) as i128) >> shift
}

/// Reads memory requested by the DWARF resolver.
fn read_memory(
    size: u8,
    memory: &mut dyn MemoryInterface,
    address: u64,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    /// Reads `SIZE` bytes from the memory.
    fn read<const SIZE: usize>(
        memory: &mut dyn MemoryInterface,
        address: u64,
    ) -> Result<[u8; SIZE], DebugError> {
        let mut buff = [0u8; SIZE];
        memory.read(address, &mut buff).map_err(|error| {
            DebugError::WarnAndContinue {
                message: format!("Unexpected error while reading debug expressions from target memory: {error:?}. Please report this as a bug.")
            }
        })?;
        Ok(buff)
    }

    let val = match size {
        1 => {
            let buff = read::<1>(memory, address)?;
            gimli::Value::U8(buff[0])
        }
        2 => {
            let buff = read::<2>(memory, address)?;
            gimli::Value::U16(u16::from_le_bytes(buff))
        }
        4 => {
            let buff = read::<4>(memory, address)?;
            gimli::Value::U32(u32::from_le_bytes(buff))
        }
        8 => {
            let buff = read::<8>(memory, address)?;
            gimli::Value::U64(u64::from_le_bytes(buff))
        }
        x => {
            return Err(DebugError::WarnAndContinue {
                message: format!(
                    "Unimplemented: Requested memory with size {x}, which is not supported yet."
                ),
            });
        }
    };

    Ok(evaluation.resume_with_memory(val)?)
}

/// A `DW_AT_start_scope` constant is an offset from the first address of the enclosing scope.
fn start_scope_constant_is_active(program_counter: u64, scope_begin: u64, offset: u64) -> bool {
    program_counter
        .checked_sub(scope_begin)
        .is_some_and(|relative| relative >= offset)
}

fn die_contains_pc(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<GimliReader>,
    entry: &gimli::DebuggingInformationEntry<GimliReader>,
    program_counter: u64,
) -> Result<bool, DebugError> {
    let mut ranges = debug_info.dwarf.die_ranges(unit, entry)?;
    Ok(loop {
        match ranges.next()? {
            Some(range) if range.contains(program_counter) => break true,
            Some(_) => {}
            None => break false,
        }
    })
}

pub(crate) trait RangeExt {
    fn contains(self, addr: u64) -> bool;
}

impl RangeExt for &mut gimli::RngListIter<GimliReader> {
    fn contains(self, addr: u64) -> bool {
        while let Ok(Some(range)) = self.next() {
            if range.contains(addr) {
                return true;
            }
        }

        false
    }
}

impl RangeExt for gimli::Range {
    fn contains(self, addr: u64) -> bool {
        self.begin <= addr && addr < self.end
    }
}

#[cfg(test)]
mod test {
    use super::{object_at, start_scope_constant_is_active};

    #[test]
    fn a_null_pointer_points_at_no_object() {
        assert_eq!(object_at(0, Some(4), Some(32)), None);
        assert_eq!(object_at(0, None, None), None);
    }

    #[test]
    fn a_pointer_to_a_zero_sized_type_points_at_no_object() {
        assert_eq!(object_at(0x2000_0000, Some(1), Some(0)), None);
        assert_eq!(object_at(1, Some(1), Some(0)), None);
    }

    #[test]
    fn a_pointer_that_holds_the_alignment_of_the_type_points_at_no_object() {
        assert_eq!(object_at(4, Some(4), Some(32)), None);
        assert_eq!(object_at(8, Some(8), Some(8)), None);
    }

    #[test]
    fn a_pointer_that_holds_more_than_the_alignment_of_the_type_points_at_an_object() {
        assert_eq!(object_at(0x2000_0004, Some(4), Some(32)), Some(0x2000_0004));
        // An object can be at a low address, for example in the flash of a target that maps the
        // flash to address zero.
        assert_eq!(object_at(0x40, Some(4), Some(64)), Some(0x40));
    }

    #[test]
    fn a_pointer_that_is_not_aligned_to_the_type_points_at_no_object() {
        assert_eq!(object_at(1, Some(4), Some(32)), None);
        assert_eq!(object_at(0x2000_0005, Some(4), Some(32)), None);
    }

    #[test]
    fn without_the_alignment_a_pointer_that_holds_no_more_than_the_size_of_the_type_points_at_no_object()
     {
        assert_eq!(object_at(1, None, Some(1)), None);
        assert_eq!(object_at(16, None, Some(32)), None);
    }

    #[test]
    fn without_the_alignment_an_address_that_no_alignment_can_be_points_at_an_object() {
        // Greater than the type.
        assert_eq!(object_at(8, None, Some(4)), Some(8));
        // Not a power of two.
        assert_eq!(object_at(12, None, Some(32)), Some(12));
        // Greater than the largest alignment of a target type.
        assert_eq!(object_at(32, None, Some(1024)), Some(32));
        // Without the size of the type, a small power of two can still be a dangling pointer.
        assert_eq!(object_at(4, None, None), None);
    }

    #[test]
    fn a_constant_start_scope_is_active_from_the_offset_of_the_enclosing_scope() {
        assert!(!start_scope_constant_is_active(0x1000, 0x1000, 0x20));
        assert!(!start_scope_constant_is_active(0x101F, 0x1000, 0x20));
        assert!(start_scope_constant_is_active(0x1020, 0x1000, 0x20));
        assert!(start_scope_constant_is_active(0x10FF, 0x1000, 0x20));
        assert!(!start_scope_constant_is_active(0x0FFF, 0x1000, 0x20));
    }
}
