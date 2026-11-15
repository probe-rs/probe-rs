use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::OnceLock,
};

use super::{
    DebugError, SourceLocation, VariableCache, debug_info::*, extract_byte_size, extract_file,
    extract_line, function_die::FunctionDie, variable::*,
};
use crate::{ObjectRef, language, stack_frame::StackFrameInfo};
use gimli::{AttributeValue, DebugInfoOffset, DebuggingInformationEntry, DwAt, UnitOffset};
use probe_rs::MemoryInterface;

/// Replace `variable` with the copy the walk left in the `cache`.
fn refresh(variable: &mut Variable, cache: &VariableCache) -> Result<(), DebugError> {
    let key = variable.variable_key;
    *variable = cache
        .get_variable_by_key(key)
        .ok_or_else(|| DebugError::Other(format!("Failed to find variable {key:?}.")))?;
    Ok(())
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
    line_sequences: OnceLock<Result<LineSequences, gimli::Error>>,
}

/// A line program that ran to completion, and its sequences of rows.
pub(crate) type LineSequences = (
    gimli::CompleteLineProgram<GimliReader>,
    Vec<gimli::LineSequence<GimliReader>>,
);

mod call_site;
mod location;
mod pointer;
mod types;
mod walk;

use call_site::{CallSiteParameter, collect_call_site};
pub(crate) use location::ExpressionResult;

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
            line_sequences: OnceLock::new(),
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
            if let Some(offset) = self.dfs_parent(prev_offset, current.depth() - previous_depth) {
                self.parents.insert(current.offset(), offset);
            }
            previous_depth = current.depth();
            prev_offset = Some(current.offset());

            for range in self.subprogram_ranges(current, dwarf) {
                self.function_dies.push((range, current.offset()));
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

    /// The parent of the next DIE in a depth-first walk. `prev_offset` is the previous DIE, and
    /// `depth_change` the depth of the next DIE minus the depth of the previous one.
    fn dfs_parent(
        &self,
        prev_offset: Option<UnitOffset>,
        depth_change: isize,
    ) -> Option<UnitOffset> {
        match depth_change {
            // Previous die is our parent.
            1 => prev_offset,
            // If 0:  Previous die is a sibling, we have the same parent.
            // If <0: Previous die is a child of one of our siblings. Trace back as many levels as needed, and grab the parent.
            levels if levels <= 0 => {
                let mut cursor = prev_offset.map(|off| self.parent_offset(off))?;
                for _ in levels..0 {
                    cursor = cursor.map(|off| self.parent_offset(off))?;
                }
                cursor
            }
            _ => unreachable!("DFS algorithms never jump down multiple levels in the graph"),
        }
    }

    /// The address ranges of `die` if it is a function.
    fn subprogram_ranges(
        &self,
        die: &DebuggingInformationEntry<GimliReader>,
        dwarf: &gimli::Dwarf<GimliReader>,
    ) -> Vec<Range<u64>> {
        if die.tag() != gimli::DW_TAG_subprogram {
            return Vec::new();
        }
        match FunctionDie::function_ranges(die, self, dwarf) {
            Ok(Some(ranges)) => ranges,
            _ => Vec::new(),
        }
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
        let value = match enum_const_value(&child_variable.memory_location, byte_size, memory)? {
            Some(value) => {
                let enumerator = find_enumerator(&enumerator_values, value, byte_size);
                self.language
                    .format_enum_value(&child_variable.type_name, &enumerator)
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
            let expression_result = match self
                .location_evaluator(debug_info, memory, frame_info)
                .extract_location(
                    node_die,
                    &parent_variable.memory_location,
                    child_variable.byte_size,
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
        let bitfield = match extract_bitfield_info(entry) {
            Ok(Some(bitfield)) => bitfield,
            Ok(None) => return Ok(()),
            Err(message) => {
                child_variable.set_value(VariableValue::Error(message));
                return Ok(());
            }
        };
        let Some(byte_size) = child_variable.byte_size else {
            child_variable.set_value(VariableValue::Error(
                "Error: Failed to decode bitfield information: byte_size not found".to_string(),
            ));
            return Ok(());
        };

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

        Ok(())
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

        let line = entry
            .attr_value(gimli::DW_AT_decl_line)
            .and_then(extract_line);
        let column = entry
            .attr(gimli::DW_AT_decl_column)
            .and_then(|attr| attr.udata_value())
            // According to the DWARF standard, a value of 0 means no column is specified.
            .filter(|&column| column != 0)
            .map(super::ColumnType::Column);

        Ok(Some(SourceLocation {
            path,
            line,
            column,
            address: None,
        }))
    }

    pub(crate) fn parent_offset(&self, offset: UnitOffset) -> Option<UnitOffset> {
        self.parents.get(&offset).copied()
    }

    /// The line program of this unit run to completion, or `None` if the unit has no line
    /// program.
    pub(crate) fn line_sequences(&self) -> Option<Result<&LineSequences, gimli::Error>> {
        let line_program = self.unit.line_program.as_ref()?;
        let sequences = self
            .line_sequences
            .get_or_init(|| line_program.clone().sequences());
        Some(sequences.as_ref().map_err(|error| *error))
    }
}

/// The bitfield layout of a member. `None` if the member has no `DW_AT_bit_size`. An error
/// message if an attribute has an unexpected form.
fn extract_bitfield_info(
    entry: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Result<Option<Bitfield>, String> {
    let offset = match udata_attribute(entry, gimli::DW_AT_data_bit_offset)? {
        // Available since DWARF 4+
        Some(offset) => Some(BitOffset::FromLsb(offset)),
        // Deprecated in DWARF 5, but still used by some compilers.
        // Specifies offset from MSB. We're handling this as a separate offset variant
        // because we haven't yet processed the byte size of the variable.
        None => udata_attribute(entry, gimli::DW_AT_bit_offset)?.map(BitOffset::FromMsb),
    };

    // Without a bit size this is not a bitfield, but a member at a byte offset.
    let Some(length) = udata_attribute(entry, gimli::DW_AT_bit_size)? else {
        return Ok(None);
    };

    Ok(Some(Bitfield {
        length,
        offset: offset.unwrap_or(BitOffset::FromLsb(0)),
    }))
}

/// The value of the unsigned attribute `name`. An error message if the attribute has another
/// form.
fn udata_attribute(
    entry: &gimli::DebuggingInformationEntry<GimliReader>,
    name: DwAt,
) -> Result<Option<u64>, String> {
    let Some(attr) = entry.attr(name) else {
        return Ok(None);
    };
    match attr.value().udata_value() {
        Some(value) => Ok(Some(value)),
        None => Err(format!(
            "Unimplemented: Attribute Value for {name}: {:?}",
            attr.value()
        )),
    }
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

/// The integer value of an enum at `location`, if the location holds one.
fn enum_const_value(
    location: &VariableLocation,
    byte_size: usize,
    memory: &mut dyn MemoryInterface,
) -> Result<Option<u128>, DebugError> {
    let value = match location {
        VariableLocation::Address(address) => {
            let mut buff = [0u8; 16];
            memory.read(*address, &mut buff[..byte_size])?;
            Some(u128::from_le_bytes(buff))
        }
        VariableLocation::RegisterValue(register_value) => {
            TryInto::<u128>::try_into(*register_value)
                .ok()
                .map(|value| truncate(value, byte_size))
        }
        _ => None,
    };
    Ok(value)
}

/// The name of the enumerator with `value`, or an `<Error: Unresolved enum value ...>` name if
/// no enumerator has it.
fn find_enumerator(
    enumerator_values: &[(VariableName, VariableValue)],
    value: u128,
    byte_size: usize,
) -> VariableName {
    // The enumerators may be signed or unsigned, so accept either reading.
    let as_signed = sign_extend(value, byte_size);
    let found = enumerator_values.iter().find(|(_name, enumerator)| {
        let VariableValue::Valid(enumerator) = enumerator else {
            return false;
        };
        enumerator
            .parse::<u128>()
            .is_ok_and(|parsed| parsed == value)
            || enumerator
                .parse::<i128>()
                .is_ok_and(|parsed| parsed == as_signed)
    });
    match found {
        Some((name, _value)) => name.clone(),
        None => VariableName::Named(format!("<Error: Unresolved enum value {value}>")),
    }
}

/// Keeps the lowest `byte_size` bytes of `value`.
fn truncate(value: u128, byte_size: usize) -> u128 {
    let shift = 128 - byte_size * 8;
    (value << shift) >> shift
}

/// Interprets the lowest `byte_size` bytes of `value` as a signed number.
fn sign_extend(value: u128, byte_size: usize) -> i128 {
    let shift = 128 - byte_size * 8;
    ((value << shift) as i128) >> shift
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

impl RangeExt for &mut gimli::RangeIter<GimliReader> {
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
    use super::start_scope_constant_is_active;

    #[test]
    fn a_constant_start_scope_is_active_from_the_offset_of_the_enclosing_scope() {
        assert!(!start_scope_constant_is_active(0x1000, 0x1000, 0x20));
        assert!(!start_scope_constant_is_active(0x101F, 0x1000, 0x20));
        assert!(start_scope_constant_is_active(0x1020, 0x1000, 0x20));
        assert!(start_scope_constant_is_active(0x10FF, 0x1000, 0x20));
        assert!(!start_scope_constant_is_active(0x0FFF, 0x1000, 0x20));
    }
}
