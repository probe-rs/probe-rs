use super::{
    debug_info::*, extract_byte_size, extract_file, extract_line, function_die::FunctionDie,
    variable::*, DebugError, DebugRegisters, EndianReader, VariableCache,
};
use crate::{
    debug::{language, stack_frame::StackFrameInfo},
    MemoryInterface,
};
use gimli::{AttributeValue, EvaluationResult, Location, UnitOffset};
use num_traits::Zero;

/// The result of `UnitInfo::evaluate_expression()` can be the value of a variable, or a memory location.
pub(crate) enum ExpressionResult {
    Value(VariableValue),
    Location(VariableLocation),
}

/// A struct containing information about a single compilation unit.
pub struct UnitInfo {
    pub(crate) unit: gimli::Unit<GimliReader, usize>,
    dwarf_language: gimli::DwLang,
    language: Box<dyn language::ProgrammingLanguage>,
}

impl UnitInfo {
    /// Create a new `UnitInfo` from a `gimli::Unit`.
    pub fn new(unit: gimli::Unit<GimliReader, usize>) -> Self {
        let dwarf_language = if let Ok(Some(AttributeValue::Language(unit_language))) = unit
            .header
            .entries_tree(&unit.abbreviations, None)
            .and_then(|mut tree| tree.root()?.entry().attr_value(gimli::DW_AT_language))
        {
            unit_language
        } else {
            tracing::warn!("Unable to retrieve DW_AT_language attribute, assuming Rust.");
            gimli::DW_LANG_Rust
        };

        Self {
            unit,
            dwarf_language,
            language: language::from_dwarf(dwarf_language),
        }
    }

    /// Retrieve the value of the `DW_AT_language` attribute of the compilation unit.
    ///
    /// In the unlikely event that we are unable to retrieve the language, we assume Rust.
    pub(crate) fn get_language(&self) -> gimli::DwLang {
        self.dwarf_language
    }

    /// Get the DIEs for the function containing the given address.
    ///
    /// If `find_inlined` is `false`, then the result will contain a single [`FunctionDie`]
    /// If `find_inlined` is `true`, then the result will contain a  [`Vec<FunctionDie>`], where the innermost (deepest in the stack) function die is the last entry in the Vec.
    pub(crate) fn get_function_dies(
        &self,
        debug_info: &super::DebugInfo,
        address: u64,
        find_inlined: bool,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        tracing::trace!("Searching Function DIE for address {:#x}", address);

        let mut entries_cursor = self.unit.entries();
        while let Ok(Some((_depth, current))) = entries_cursor.next_dfs() {
            let Some(mut die) = FunctionDie::new(current.clone(), self) else {
                continue;
            };

            let mut ranges = debug_info.dwarf.die_ranges(&self.unit, current)?;

            while let Ok(Some(range)) = ranges.next() {
                if !range.contains(address) {
                    continue;
                }

                // Check if we are actually in an inlined function
                die.low_pc = range.begin;
                die.high_pc = range.end;

                // Extract the frame_base for this function DIE.
                let mut functions = vec![die];

                tracing::debug!(
                    "Found DIE: name={:?}",
                    functions[0].function_name(debug_info)
                );

                if find_inlined {
                    tracing::debug!("Checking for inlined functions");

                    let inlined_functions =
                        self.find_inlined_functions(debug_info, address, current.offset())?;

                    if inlined_functions.is_empty() {
                        tracing::debug!("No inlined function found!");
                    } else {
                        tracing::debug!(
                            "{} inlined functions for address {}",
                            inlined_functions.len(),
                            address
                        );
                    }

                    functions.extend(inlined_functions.into_iter());
                }
                return Ok(functions);
            }
        }
        Ok(vec![])
    }

    /// Check if the function located at the given offset contains inlined functions at the
    /// given address.
    pub(crate) fn find_inlined_functions(
        &self,
        debug_info: &DebugInfo,
        address: u64,
        offset: UnitOffset,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        // If we don't have any entries at our unit offset, return an empty vector.
        let Ok(mut cursor) = self.unit.entries_at_offset(offset) else {
            return Ok(vec![]);
        };

        let mut current_depth = 0;
        let mut abort_depth = 0;
        let mut functions = Vec::new();

        while let Ok(Some((depth, current))) = cursor.next_dfs() {
            current_depth += depth;
            if current_depth < abort_depth {
                break;
            }

            // Skip anything that is not an inlined subroutine.
            if current.tag() != gimli::DW_TAG_inlined_subroutine {
                continue;
            }

            let mut ranges = debug_info.dwarf.die_ranges(&self.unit, current)?;

            while let Ok(Some(range)) = ranges.next() {
                if !range.contains(address) {
                    continue;
                }
                // Check if we are actually in an inlined function

                // We don't have to search further up in the tree, if there are multiple inlined functions,
                // they will be children of the current function.
                abort_depth = current_depth;

                // Find the abstract definition
                let Ok(Some(abstract_origin)) = current.attr(gimli::DW_AT_abstract_origin) else {
                    tracing::warn!("No abstract origin for inlined function, skipping.");
                    return Ok(vec![]);
                };
                let abstract_origin_value = abstract_origin.value();
                let gimli::AttributeValue::UnitRef(unit_ref) = abstract_origin_value else {
                    tracing::warn!(
                        "Unsupported DW_AT_abstract_origin value: {:?}",
                        abstract_origin_value
                    );
                    continue;
                };

                let Some(mut die) = self.unit.entry(unit_ref).ok().and_then(|abstract_die| {
                    FunctionDie::new_inlined(current.clone(), abstract_die.clone(), self)
                }) else {
                    continue;
                };

                die.low_pc = range.begin;
                die.high_pc = range.end;

                functions.push(die);
            }
        }

        Ok(functions)
    }

    /// Recurse the ELF structure below the `tree_node`, and ...
    /// - Consumes the `child_variable`.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_tree_node_attributes(
        &self,
        debug_info: &DebugInfo,
        tree_node: &mut gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &mut Variable,
        mut child_variable: Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<Variable, DebugError> {
        // Identify the parent.
        child_variable.parent_key = parent_variable.variable_key;

        // We need to determine if we are working with a 'abstract` location, and use that node for the attributes we need
        let attributes_entry = if let Ok(Some(abstract_origin)) =
            tree_node.entry().attr(gimli::DW_AT_abstract_origin)
        {
            match abstract_origin.value() {
                gimli::AttributeValue::UnitRef(unit_ref) => {
                    // The abstract origin is a reference to another DIE, so we need to resolve that,
                    // but first we need to process the (optional) memory location using the current DIE.
                    self.process_memory_location(
                        debug_info,
                        tree_node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        frame_info,
                    )?;
                    Some(
                        self.unit
                            .header
                            .entries_tree(&self.unit.abbreviations, Some(unit_ref))?
                            .root()?
                            .entry()
                            .clone(),
                    )
                }
                other_attribute_value => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "Unimplemented: Attribute Value for DW_AT_abstract_origin {other_attribute_value:?}"
                    )));
                    None
                }
            }
        } else {
            Some(tree_node.entry().clone())
        };

        // For variable attribute resolution, we need to resolve a few attributes in advance of looping through all the other ones.
        // Try to exact the name first, for easier debugging
        if let Some(entry) = attributes_entry.as_ref() {
            if let Ok(Some(name)) = extract_name(debug_info, entry) {
                child_variable.name = VariableName::Named(name);
            }
        }

        if let Some(attributes_entry) = attributes_entry {
            let mut variable_attributes = attributes_entry.attrs();

            // Now loop through all the unit attributes to extract the remainder of the `Variable` definition.
            while let Ok(Some(attr)) = variable_attributes.next() {
                match attr.name() {
                    gimli::DW_AT_location | gimli::DW_AT_data_member_location => {
                        // The child_variable.location is calculated with attribute gimli::DW_AT_type, to ensure it gets done before DW_AT_type is processed
                    }
                    gimli::DW_AT_name => {
                        // This was done before we started looping through attributes, so we can ignore it.
                    }
                    gimli::DW_AT_decl_file => {
                        if let Some((directory, file_name)) =
                            extract_file(debug_info, &self.unit, attr.value())
                        {
                            child_variable.source_location.file = Some(file_name);
                            child_variable.source_location.directory = Some(directory);
                        }
                    }
                    gimli::DW_AT_decl_line => {
                        if let Some(line_number) = extract_line(attr.value()) {
                            child_variable.source_location.line = Some(line_number);
                        }
                    }
                    gimli::DW_AT_decl_column => {
                        // Unused.
                    }
                    gimli::DW_AT_containing_type => {
                        // TODO: Implement [documented RUST extensions to DWARF standard](https://rustc-dev-guide.rust-lang.org/debugging-support-in-rustc.html?highlight=dwarf#dwarf-and-rustc)
                    }
                    gimli::DW_AT_type => {
                        // The rules to calculate the type of a child variable are complex, and depend on a number of other attributes.
                        // Depending on the presence and value of these attributes, the [Variable::memory_location] may need to be calculated differently.
                        // - The `DW_AT_type` of the parent (e.g. is it a pointer, or a struct, or an array, etc.).
                        // - The `DW_AT_address_class of the child (we need to know if it is present, and if it has a value of 0 - unspecified)
                        // - The `DW_AT_data_member_location` of the child.
                        // - The `DW_AT_location` of the child.
                        // - The `DW_AT_byte_size` of the child.
                        // - The `DW_AT_name` of the data type node.
                        child_variable = self.process_type_attribute(
                            &attr,
                            debug_info,
                            &attributes_entry,
                            parent_variable,
                            child_variable,
                            memory,
                            frame_info,
                            cache,
                        )?;
                    }
                    gimli::DW_AT_enum_class => match attr.value() {
                        gimli::AttributeValue::Flag(true) => {
                            child_variable.set_value(VariableValue::Valid(format!(
                                "{:?}",
                                child_variable.type_name
                            )));
                        }
                        gimli::AttributeValue::Flag(false) => {
                            child_variable.set_value(VariableValue::Error(
                                "Unimplemented: DW_AT_enum_class(false)".to_string(),
                            ));
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_enum_class: {other_attribute_value:?}"
                            )));
                        }
                    },
                    gimli::DW_AT_const_value => {
                        let attr_value = attr.value();
                        let variable_value = if let Some(const_value) = attr_value.udata_value() {
                            VariableValue::Valid(const_value.to_string())
                        } else if let Some(const_value) = attr_value.sdata_value() {
                            VariableValue::Valid(const_value.to_string())
                        } else {
                            VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_const_value: {:?}",
                                attr_value
                            ))
                        };

                        child_variable.set_value(variable_value)
                    }
                    gimli::DW_AT_alignment => {
                        // TODO: Figure out when (if at all) we need to do anything with DW_AT_alignment for the purposes of decoding data values.
                    }
                    gimli::DW_AT_artificial => {
                        // These are references for entries like discriminant values of `VariantParts`.
                        child_variable.name = VariableName::Artifical;
                    }
                    gimli::DW_AT_discr => match attr.value() {
                        // This calculates the active discriminant value for the `VariantPart`.
                        gimli::AttributeValue::UnitRef(unit_ref) => {
                            let mut type_tree = self
                                .unit
                                .header
                                .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                            let mut discriminant_node = type_tree.root()?;
                            let mut discriminant_variable = cache.create_variable(
                                parent_variable.variable_key,
                                Some(discriminant_node.entry().offset()),
                                Some(self),
                            )?;
                            discriminant_variable = self.process_tree_node_attributes(
                                debug_info,
                                &mut discriminant_node,
                                parent_variable,
                                discriminant_variable,
                                memory,
                                cache,
                                frame_info,
                            )?;

                            let variant_part = if discriminant_variable.is_valid() {
                                discriminant_variable
                                    .get_value(cache)
                                    .parse()
                                    .unwrap_or(u64::MAX)
                            } else {
                                u64::MAX
                            };

                            parent_variable.role = VariantRole::VariantPart(variant_part);
                            cache.remove_cache_entry(discriminant_variable.variable_key)?;
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_discr {other_attribute_value:?}"
                            )));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_lower_bound => match attr.value().udata_value() {
                        Some(bound) => child_variable.range_lower_bound = bound as i64,
                        None => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_lower_bound: {:?}",
                                attr.value()
                            )));
                        }
                    },
                    // Property of variables that are of DW_TAG_subrange_type.
                    gimli::DW_AT_upper_bound | gimli::DW_AT_count => {
                        match attr.value().udata_value() {
                            Some(bound) => child_variable.range_upper_bound = bound as i64,
                            None => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_upper_bound: {:?}",
                                    attr.value()
                                )));
                            }
                        }
                    }
                    gimli::DW_AT_accessibility => {
                        // Silently ignore these for now.
                        // TODO: Add flag for public/private/protected for `Variable`, once we have a use case.
                    }
                    gimli::DW_AT_external => {
                        // TODO: Implement globally visible variables.
                    }
                    gimli::DW_AT_declaration => {
                        // Unimplemented.
                    }
                    gimli::DW_AT_encoding => {
                        // Ignore these. RUST data types handle this intrinsicly.
                    }
                    gimli::DW_AT_discr_value => {
                        // Processed by `extract_variant_discriminant()`.
                    }
                    gimli::DW_AT_byte_size => {
                        // Processed by `extract_byte_size()`.
                    }
                    gimli::DW_AT_abstract_origin => {
                        // Processed before looping through all attributes
                    }
                    gimli::DW_AT_linkage_name => {
                        // Unused attribute of, for example, inlined DW_TAG_subroutine
                    }
                    gimli::DW_AT_address_class => {
                        // Processed by `extract_type()`
                    }
                    other_attribute => {
                        // This follows the examples of the "format!" documenation as the way to limit string length of a {:?} parameter.
                        child_variable.set_value(VariableValue::Error(format!(
                            "Unimplemented: Variable Attribute {:.100} : {:.100}, with children = {}",
                            format!("{:?}", other_attribute.static_string()),
                            format!("{:?}", attributes_entry.attr_value(other_attribute)),
                            attributes_entry.has_children()
                        )));
                    }
                }
            }
        }
        cache.update_variable_and_value(&mut child_variable, memory)?;

        Ok(child_variable)
    }

    #[allow(clippy::too_many_arguments)]
    fn process_type_attribute(
        &self,
        attr: &gimli::Attribute<GimliReader>,
        debug_info: &DebugInfo,
        attributes_entry: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        memory: &mut dyn MemoryInterface,
        frame_info: StackFrameInfo<'_>,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        match attr.value() {
            gimli::AttributeValue::UnitRef(unit_ref) => {
                // Reference to a type, or an entry to another type or a type modifier which will point to another type.
                // Before we resolve that type tree, we need to resolve the current node's memory location.
                // This is because the memory location of the type nodes and child variables often inherit this value.
                self.process_memory_location(
                    debug_info,
                    attributes_entry,
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;

                // Now reslolve the referenced tree node for the type.
                let mut type_tree = self
                    .unit
                    .header
                    .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                let referenced_type_tree_node = type_tree.root()?;
                child_variable = self.extract_type(
                    debug_info,
                    referenced_type_tree_node,
                    parent_variable,
                    child_variable,
                    memory,
                    cache,
                    frame_info,
                )?;
            }
            other_attribute_value => {
                child_variable.set_value(VariableValue::Error(format!(
                    "Unimplemented: Attribute Value for DW_AT_type {other_attribute_value:?}"
                )));
            }
        }

        Ok(child_variable)
    }

    /// Recurse the ELF structure below the `parent_node`, and ...
    /// - Consumes the `parent_variable`.
    /// - Updates the `DebugInfo::VariableCache` with all descendant `Variable`s.
    /// - Returns a clone of the most up-to-date `parent_variable` in the cache.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_tree(
        &self,
        debug_info: &DebugInfo,
        parent_node: gimli::EntriesTreeNode<GimliReader>,
        mut parent_variable: Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<Variable, DebugError> {
        if !parent_variable.is_valid() {
            cache.update_variable_and_value(&mut parent_variable, memory)?;
            return Ok(parent_variable);
        }

        let Some(program_counter) = frame_info
            .registers
            .get_program_counter()
            .and_then(|reg| reg.value)
        else {
            return Err(DebugError::UnwindIncompleteResults {
                message: "Cannot unwind `Variable` without a valid PC (program_counter)"
                    .to_string(),
            });
        };
        let program_counter = program_counter.try_into()?;

        tracing::trace!("process_tree for parent {:?}", parent_variable.variable_key);

        let mut child_nodes = parent_node.children();
        while let Some(mut child_node) = child_nodes.next()? {
            match child_node.entry().tag() {
                gimli::DW_TAG_namespace => {
                    // Use these parents to extract `statics`.
                    let mut namespace_variable =
                        Variable::new(Some(child_node.entry().offset()), Some(self));

                    namespace_variable.name =
                        if let Ok(Some(name)) = extract_name(debug_info, child_node.entry()) {
                            VariableName::Namespace(name)
                        } else {
                            VariableName::AnonymousNamespace
                        };
                    namespace_variable.type_name = VariableType::Namespace;
                    namespace_variable.memory_location = VariableLocation::Unavailable;
                    cache.add_variable(parent_variable.variable_key, &mut namespace_variable)?;

                    // Recurse for additional namespace variables.
                    namespace_variable = self.process_tree(
                        debug_info,
                        child_node,
                        namespace_variable,
                        memory,
                        cache,
                        frame_info,
                    )?;

                    // Do not keep empty namespaces around
                    if !cache.has_children(&namespace_variable)? {
                        cache.remove_cache_entry(namespace_variable.variable_key)?;
                    }
                }

                gimli::DW_TAG_formal_parameter
                | gimli::DW_TAG_variable
                | gimli::DW_TAG_member
                | gimli::DW_TAG_enumerator => {
                    // This branch handles:
                    //  - Parameters to functions.
                    //  - Typical top-level variables.
                    //  - Members of structured types.
                    //  - Possible values for enumerators, used by extract_type() when processing DW_TAG_enumeration_type.
                    let mut child_variable = cache.create_variable(
                        parent_variable.variable_key,
                        Some(child_node.entry().offset()),
                        Some(self),
                    )?;
                    child_variable = self.process_tree_node_attributes(
                        debug_info,
                        &mut child_node,
                        &mut parent_variable,
                        child_variable,
                        memory,
                        cache,
                        frame_info,
                    )?;
                    // Do not keep or process PhantomData nodes, or variant parts that we have already used.
                    if child_variable.type_name.is_phantom_data()
                        || child_variable.name == VariableName::Artifical
                    {
                        cache.remove_cache_entry(child_variable.variable_key)?;
                    } else if child_variable.is_valid() {
                        // Recursively process each child.
                        self.process_tree(
                            debug_info,
                            child_node,
                            child_variable,
                            memory,
                            cache,
                            frame_info,
                        )?;
                    }
                }
                gimli::DW_TAG_variant_part => {
                    // We need to recurse through the children, to find the DW_TAG_variant with discriminant matching the DW_TAG_variant,
                    // and ONLY add it's children to the parent variable.
                    // The structure looks like this (there are other nodes in the structure that we use and discard before we get here):
                    // Level 1: --> An actual variable that has a variant value
                    //      Level 2: --> this DW_TAG_variant_part node (some child nodes are used to calc the active Variant discriminant)
                    //          Level 3: --> Some DW_TAG_variant's that have discriminant values to be matched against the discriminant
                    //              Level 4: --> The actual variables, with matching discriminant, which will be added to `parent_variable`
                    // TODO: Handle Level 3 nodes that belong to a DW_AT_discr_list, instead of having a discreet DW_AT_discr_value
                    let mut child_variable = cache.create_variable(
                        parent_variable.variable_key,
                        Some(child_node.entry().offset()),
                        Some(self),
                    )?;
                    // To determine the discriminant, we use the following rules:
                    // - If there is no DW_AT_discr, then there will be a single DW_TAG_variant, and this will be the matching value. In the code here, we assign a default value of u64::MAX to both, so that they will be matched as belonging together (https://dwarfstd.org/ShowIssue.php?issue=180517.2)
                    // - TODO: The [DWARF] standard, 5.7.10, allows for a case where there is no DW_AT_discr attribute, but a DW_AT_type to represent the tag. I have not seen that generated from RUST yet.
                    // - If there is a DW_AT_discr that has a value, then this is a reference to the member entry for the discriminant. This value will be resolved to match against the appropriate DW_TAG_variant.
                    // - TODO: The [DWARF] standard, 5.7.10, allows for a DW_AT_discr_list, but I have not seen that generated from RUST yet.
                    parent_variable.role = VariantRole::VariantPart(u64::MAX);
                    child_variable = self.process_tree_node_attributes(
                        debug_info,
                        &mut child_node,
                        &mut parent_variable,
                        child_variable,
                        memory,
                        cache,
                        frame_info,
                    )?;
                    // At this point we have everything we need (It has updated the parent's `role`) from the child_variable, so elimnate it before we continue ...
                    cache.remove_cache_entry(child_variable.variable_key)?;
                    parent_variable = self.process_tree(
                        debug_info,
                        child_node,
                        parent_variable,
                        memory,
                        cache,
                        frame_info,
                    )?;
                }

                // Variant is a child of a structure, and one of them should have a discriminant value to match the DW_TAG_variant_part
                gimli::DW_TAG_variant => {
                    // We only need to do this if we have not already found our variant,
                    if !cache.has_children(&parent_variable)? {
                        let mut child_variable = cache.create_variable(
                            parent_variable.variable_key,
                            Some(child_node.entry().offset()),
                            Some(self),
                        )?;
                        self.extract_variant_discriminant(&child_node, &mut child_variable)?;
                        child_variable = self.process_tree_node_attributes(
                            debug_info,
                            &mut child_node,
                            &mut parent_variable,
                            child_variable,
                            memory,
                            cache,
                            frame_info,
                        )?;
                        if child_variable.is_valid() {
                            if let VariantRole::Variant(discriminant) = child_variable.role {
                                // Only process the discriminant variants or when we eventually   encounter the default
                                if parent_variable.role == VariantRole::VariantPart(discriminant)
                                    || discriminant == u64::MAX
                                {
                                    self.process_memory_location(
                                        debug_info,
                                        child_node.entry(),
                                        &parent_variable,
                                        &mut child_variable,
                                        memory,
                                        frame_info,
                                    )?;
                                    // Recursively process each relevant child node.
                                    child_variable = self.process_tree(
                                        debug_info,
                                        child_node,
                                        child_variable,
                                        memory,
                                        cache,
                                        frame_info,
                                    )?;
                                    if child_variable.is_valid() {
                                        // Eliminate intermediate DWARF nodes, but keep their children
                                        cache.adopt_grand_children(
                                            &parent_variable,
                                            &child_variable,
                                        )?;
                                    }
                                } else {
                                    cache.remove_cache_entry(child_variable.variable_key)?;
                                }
                            }
                        } else {
                            cache.remove_cache_entry(child_variable.variable_key)?;
                        }
                    }
                }
                gimli::DW_TAG_subrange_type => {
                    // This tag is a child node fore parent types such as (array, vector, etc.).
                    // Recursively process each node, but pass the parent_variable so that new children are caught despite missing these tags.
                    let mut range_variable = cache.create_variable(
                        parent_variable.variable_key,
                        Some(child_node.entry().offset()),
                        Some(self),
                    )?;

                    range_variable = self.process_tree_node_attributes(
                        debug_info,
                        &mut child_node,
                        &mut parent_variable,
                        range_variable,
                        memory,
                        cache,
                        frame_info,
                    )?;
                    // Determine if we should use the results ...
                    if range_variable.is_valid() {
                        // Pass the pertinent info up to the parent_variable.
                        parent_variable.type_name = range_variable.type_name;
                        parent_variable.range_lower_bound = range_variable.range_lower_bound;
                        parent_variable.range_upper_bound = range_variable.range_upper_bound;
                    }
                    cache.remove_cache_entry(range_variable.variable_key)?;
                }
                gimli::DW_TAG_lexical_block => {
                    // Determine the low and high ranges for which this DIE and children are in scope. These can be specified discreetly, or in ranges.
                    let mut in_scope = false;
                    if let Ok(Some(low_pc_attr)) = child_node.entry().attr(gimli::DW_AT_low_pc) {
                        let low_pc = match low_pc_attr.value() {
                            gimli::AttributeValue::Addr(value) => value,
                            _other => u64::MAX,
                        };
                        let high_pc = if let Ok(Some(high_pc_attr)) =
                            child_node.entry().attr(gimli::DW_AT_high_pc)
                        {
                            match high_pc_attr.value() {
                                gimli::AttributeValue::Addr(addr) => addr,
                                gimli::AttributeValue::Udata(unsigned_offset) => {
                                    low_pc + unsigned_offset
                                }
                                _other => 0_u64,
                            }
                        } else {
                            0_u64
                        };
                        if low_pc == u64::MAX || high_pc == 0_u64 {
                            // These have not been specified correctly ... something went wrong.
                            parent_variable.set_value(VariableValue::Error("Error: Processing of variables failed because of invalid/unsupported scope information. Please log a bug at 'https://github.com/probe-rs/probe-rs/issues'".to_string()));
                        }
                        let block_range = gimli::Range {
                            begin: low_pc,
                            end: high_pc,
                        };
                        if block_range.contains(program_counter) {
                            // We have established positive scope, so no need to continue.
                            in_scope = true;
                        }
                        // No scope info yet, so keep looking.
                    };
                    // Searching for ranges has a bit more overhead, so ONLY do this if do not have scope confirmed yet.
                    if !in_scope {
                        if let Ok(Some(ranges)) = child_node.entry().attr(gimli::DW_AT_ranges) {
                            match ranges.value() {
                                gimli::AttributeValue::RangeListsRef(raw_range_lists_offset) => {
                                    let range_lists_offset = debug_info
                                        .dwarf
                                        .ranges_offset_from_raw(&self.unit, raw_range_lists_offset);

                                    if let Ok(mut range_iter) =
                                        debug_info.dwarf.ranges(&self.unit, range_lists_offset)
                                    {
                                        in_scope = range_iter.contains(program_counter);
                                    }
                                }
                                other_range_attribute => {
                                    let error = format!(
                                        "Found unexpected scope attribute: {:?} for variable {:?}",
                                        other_range_attribute, parent_variable.name
                                    );
                                    parent_variable.set_value(VariableValue::Error(error));
                                }
                            }
                        }
                    }
                    if in_scope {
                        // This is IN scope.
                        // Recursively process each child, but pass the parent_variable, so that we don't create intermediate nodes for scope identifiers.
                        parent_variable = self.process_tree(
                            debug_info,
                            child_node,
                            parent_variable,
                            memory,
                            cache,
                            frame_info,
                        )?;
                    } else {
                        // This lexical block is NOT in scope, but other children of this parent may well be in scope, so do NOT invalidate the parent_variable.
                    }
                }
                gimli::DW_TAG_template_type_parameter => {
                    // The parent node for Rust generic type parameter
                    // These show up as a child of structures they belong to and points to the type that matches the template.
                    // They are followed by a sibling of `DW_TAG_member` with name '__0' that has all the attributes needed to resolve the value.
                    // TODO: If there are multiple types supported, then I suspect there will be additional `DW_TAG_member` siblings. We will need to match those correctly.
                }

                // Inlined subroutines are handled at the StackFame level
                gimli::DW_TAG_inlined_subroutine
                | gimli::DW_TAG_base_type
                | gimli::DW_TAG_pointer_type
                | gimli::DW_TAG_structure_type
                | gimli::DW_TAG_enumeration_type
                | gimli::DW_TAG_array_type
                | gimli::DW_TAG_subroutine_type
                | gimli::DW_TAG_subprogram
                | gimli::DW_TAG_union_type => {
                    // These will be processed elsewhere, or not at all, until we discover a use case that needs to be implemented.
                }
                unimplemented => {
                    let error = format!(
                        "Unimplemented: Encountered unimplemented DwTag {:?} for Variable {:?}",
                        unimplemented.static_string(),
                        parent_variable.name
                    );
                    parent_variable.set_value(VariableValue::Error(error));
                }
            }
        }

        cache.update_variable_and_value(&mut parent_variable, memory)?;

        Ok(parent_variable)
    }

    /// Compute the discriminant value of a DW_TAG_variant variable. If it is not explicitly captured in the DWARF, then it is the default value.
    pub(crate) fn extract_variant_discriminant(
        &self,
        node: &gimli::EntriesTreeNode<GimliReader>,
        variable: &mut Variable,
    ) -> Result<(), DebugError> {
        variable.role = match node.entry().attr(gimli::DW_AT_discr_value) {
            Ok(Some(discr_value_attr)) => {
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
            Ok(None) => {
                // In the case where the variable is a DW_TAG_variant, but has NO DW_AT_discr_value, then this is the "default" to be used.
                VariantRole::Variant(u64::MAX)
            }
            Err(_error) => {
                variable.set_value(VariableValue::Error(format!(
                    "Error: Retrieving DW_AT_discr_value for variable {variable:?}"
                )));
                VariantRole::NonVariant
            }
        };

        Ok(())
    }

    /// Compute the type (base to complex) of a variable. Only base types have values.
    /// Complex types are references to node trees, that require traversal in similar ways to other DIE's like functions.
    /// This means both [`get_function_variables()`] and [`extract_type()`] will call the recursive [`process_tree()`] method to build an integrated `tree` of variables with types and values.
    /// - Consumes the `child_variable`.
    /// - Returns a clone of the most up-to-date `child_variable` in the cache.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn extract_type(
        &self,
        debug_info: &DebugInfo,
        node: gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<Variable, DebugError> {
        let type_name = match extract_name(debug_info, node.entry()) {
            Ok(name) => name,
            Err(error) => {
                let message = format!("Error: evaluating type name: {error:?} ");
                child_variable.set_value(VariableValue::Error(message.clone()));
                Some(message)
            }
        };

        if !child_variable.is_valid() {
            cache.update_variable_and_value(&mut child_variable, memory)?;

            return Ok(child_variable);
        }

        match node.entry().tag() {
            gimli::DW_TAG_base_type => {
                child_variable.type_name =
                    VariableType::Base(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                self.process_memory_location(
                    debug_info,
                    node.entry(),
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;
            }
            gimli::DW_TAG_pointer_type => {
                child_variable.type_name = VariableType::Pointer(type_name);
                self.process_memory_location(
                    debug_info,
                    node.entry(),
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;

                // This needs to resolve the pointer before the regular recursion can continue.
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(Some(data_type_attribute)) => {
                        match data_type_attribute.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                // The default behaviour is to defer the processing of child types.
                                child_variable.variable_node_type =
                                    VariableNodeType::ReferenceOffset(unit_ref);
                                if let VariableType::Pointer(optional_name) =
                                    &child_variable.type_name
                                {
                                    #[allow(clippy::unwrap_used)]
                                    // Use of `unwrap` below is safe because we first check for `is_none()`.
                                    if optional_name.is_none()
                                        || optional_name.as_ref().unwrap().starts_with("*const")
                                        || optional_name.as_ref().unwrap().starts_with("*mut")
                                    {
                                        // Resolve the children of this variable, because they contain essential information required to resolve the value
                                        debug_info.cache_deferred_variables(
                                            cache,
                                            memory,
                                            &mut child_variable,
                                            frame_info,
                                        )?;
                                    } else {
                                        // This is the case where we defer the processing of child types.
                                    }
                                } else {
                                    debug_info.cache_deferred_variables(
                                        cache,
                                        memory,
                                        &mut child_variable,
                                        frame_info,
                                    )?;
                                }
                            }
                            other_attribute_value => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_type {:.100}",
                                    format!("{other_attribute_value:?}")
                                )));
                            }
                        }
                    }
                    Ok(None) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Error: No Attribute Value for DW_AT_type for variable {:?}",
                            child_variable.name
                        )));
                    }
                    Err(error) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Error: Failed to decode pointer reference: {error:?}"
                        )));
                    }
                }
            }
            gimli::DW_TAG_structure_type => {
                child_variable.type_name =
                    VariableType::Struct(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                self.process_memory_location(
                    debug_info,
                    node.entry(),
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;

                if child_variable.memory_location != VariableLocation::Unavailable {
                    if let VariableType::Struct(name) = &child_variable.type_name {
                        // The default behaviour is to defer the processing of child types.
                        child_variable.variable_node_type =
                            VariableNodeType::TypeOffset(node.entry().offset());
                        // In some cases, it really simplifies the UX if we can auto resolve the children and derive a value that is visible at first glance to the user.
                        if name.starts_with("&str")
                            || name.starts_with("Option")
                            || name.starts_with("Some")
                            || name.starts_with("Result")
                            || name.starts_with("Ok")
                            || name.starts_with("Err")
                        {
                            let temp_node_type = child_variable.variable_node_type;
                            child_variable.variable_node_type = VariableNodeType::RecurseToBaseType;
                            child_variable = self.process_tree(
                                debug_info,
                                node,
                                child_variable,
                                memory,
                                cache,
                                frame_info,
                            )?;
                            child_variable.variable_node_type = temp_node_type;
                        }
                    }
                } else {
                    // If something is already broken, then do nothing ...
                    child_variable.variable_node_type = VariableNodeType::DoNotRecurse;
                }
            }
            gimli::DW_TAG_enumeration_type => {
                child_variable.type_name =
                    VariableType::Enum(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                self.process_memory_location(
                    debug_info,
                    node.entry(),
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;

                // Recursively process a child types.
                child_variable =
                    self.process_tree(debug_info, node, child_variable, memory, cache, frame_info)?;
                if parent_variable.is_valid() && child_variable.is_valid() {
                    let enumerator_values = cache.get_children(child_variable.variable_key)?;

                    let value = if let VariableLocation::Address(address) =
                        child_variable.memory_location
                    {
                        // NOTE: hard-coding value of variable.byte_size to 1 ... replace with code if necessary.
                        let mut buff = 0u8;
                        memory.read(address, std::slice::from_mut(&mut buff))?;
                        let this_enum_const_value = buff.to_string();

                        let is_this_value = |enumerator_variable: &Variable| {
                            enumerator_variable.get_value(cache) == this_enum_const_value
                        };

                        let enumumerator_value =
                            match enumerator_values.into_iter().find(is_this_value) {
                                Some(this_enum) => this_enum.name,
                                None => VariableName::Named(
                                    "<Error: Unresolved enum value>".to_string(),
                                ),
                            };

                        self.language
                            .format_enum_value(&child_variable.type_name, &enumumerator_value)
                    } else {
                        VariableValue::Error(format!(
                            "Unsupported variable location {:?}",
                            child_variable.memory_location
                        ))
                    };

                    child_variable.set_value(value);

                    // We don't need to keep these children.
                    cache.remove_cache_entry_children(child_variable.variable_key)?;
                }
            }
            gimli::DW_TAG_array_type => {
                // This node is a pointer to the type of data stored in the array, with a direct child that contains the range information.
                // To resolve the value of an array type, we need the following:
                // 1. The memory location of the array.
                //   - The attribute for the first member of the array, is stored on the parent(array) node.
                //   - The memory location for each subsequent member is then calculated based on the DW_AT_byte_size of the child node.
                // 2. The byte size of the array.
                //   - The byte size of the array is the product of the number of elements and the byte size of the child node.
                //   - This has to be calculated from the deepest level (the DWARF only encodes it there) of multi-dimensional arrays, upwards.
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(Some(data_type_attribute)) => {
                        match data_type_attribute.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                // The memory location of array members build on top of the memory location of the child_variable.
                                self.process_memory_location(
                                    debug_info,
                                    node.entry(),
                                    parent_variable,
                                    &mut child_variable,
                                    memory,
                                    frame_info,
                                )?;
                                // Now we can explode the array members.
                                // First get the DW_TAG_subrange child of this node. It has a DW_AT_type that points to DW_TAG_base_type:__ARRAY_SIZE_TYPE__.
                                let mut subrange_variable = cache.create_variable(
                                    child_variable.variable_key,
                                    Some(node.entry().offset()),
                                    Some(self),
                                )?;
                                subrange_variable = self.process_tree(
                                    debug_info,
                                    node,
                                    subrange_variable,
                                    memory,
                                    cache,
                                    frame_info,
                                )?;
                                child_variable.range_lower_bound =
                                    subrange_variable.range_lower_bound;
                                child_variable.range_upper_bound =
                                    subrange_variable.range_upper_bound;
                                if child_variable.range_lower_bound < 0
                                    || child_variable.range_upper_bound < 0
                                {
                                    child_variable.set_value(VariableValue::Error(format!(
                                        "Unimplemented: Array has a sub-range of {}..{} for ",
                                        child_variable.range_lower_bound,
                                        child_variable.range_upper_bound
                                    )));
                                }
                                cache.remove_cache_entry(subrange_variable.variable_key)?;

                                if child_variable.subrange_bounds().count() == 0 {
                                    // Gracefully handle the case where the array is empty.
                                    // - Resolve a 'dummy' child, to determine the type of child_variable.
                                    self.expand_array_member(
                                        debug_info,
                                        unit_ref,
                                        cache,
                                        &mut child_variable,
                                        memory,
                                        0,
                                        frame_info,
                                    )?;
                                    // - Delete the dummy child that was created above.
                                    cache
                                        .remove_cache_entry_children(child_variable.variable_key)?;
                                } else {
                                    // - Next, process this DW_TAG_array_type's DW_AT_type full tree.
                                    // - We have to do this repeatedly, for every array member in the range.
                                    for array_member_index in child_variable.subrange_bounds() {
                                        self.expand_array_member(
                                            debug_info,
                                            unit_ref,
                                            cache,
                                            &mut child_variable,
                                            memory,
                                            array_member_index,
                                            frame_info,
                                        )?;
                                    }
                                }
                            }
                            other_attribute_value => {
                                child_variable.set_value(VariableValue::Error(
                                    format!(
                                        "Unimplemented: Attribute Value for DW_AT_type {other_attribute_value:?}"
                                    ),
                                ));
                            }
                        }
                    }
                    Ok(None) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Error: No Attribute Value for DW_AT_type for variable {:?}",
                            child_variable.name
                        )));
                    }
                    Err(error) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Error: Failed to decode pointer reference: {error:?}"
                        )));
                    }
                }
            }
            gimli::DW_TAG_union_type => {
                child_variable.type_name =
                    VariableType::Base(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                self.process_memory_location(
                    debug_info,
                    node.entry(),
                    parent_variable,
                    &mut child_variable,
                    memory,
                    frame_info,
                )?;
                // Recursively process a child types.
                // TODO: The DWARF does not currently hold information that allows decoding of which UNION arm is instantiated, so we have to display all available.
                child_variable =
                    self.process_tree(debug_info, node, child_variable, memory, cache, frame_info)?;
                if child_variable.is_valid() && !cache.has_children(&child_variable)? {
                    // Empty structs don't have values.
                    child_variable.set_value(VariableValue::Valid(format!(
                        "{:?}",
                        child_variable.type_name.clone()
                    )));
                }
            }
            gimli::DW_TAG_subroutine_type => {
                // The type_name will be found in the DW_AT_TYPE child of this entry.
                // NOTE: There might be value in going beyond just getting the name, but also the parameters (children) and return type (extract_type()).
                match node.entry().attr(gimli::DW_AT_type) {
                    Ok(Some(data_type_attribute)) => match data_type_attribute.value() {
                        gimli::AttributeValue::UnitRef(unit_ref) => {
                            let subroutine_type_node =
                                self.unit.header.entry(&self.unit.abbreviations, unit_ref)?;
                            child_variable.type_name =
                                match extract_name(debug_info, &subroutine_type_node) {
                                    Ok(Some(name_attr)) => VariableType::Other(name_attr),
                                    Ok(None) => VariableType::Unknown,
                                    Err(error) => VariableType::Other(format!(
                                        "Error: evaluating subroutine type name: {error:?} "
                                    )),
                                };
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_type {:.100}",
                                format!("{other_attribute_value:?}")
                            )));
                        }
                    },

                    Ok(None) => {
                        // TODO: Better indication for no return value
                        child_variable
                            .set_value(VariableValue::Valid("<No Return Value>".to_string()));
                        child_variable.type_name = VariableType::Unknown;
                    }

                    Err(error) => {
                        child_variable.set_value(VariableValue::Error(format!(
                            "Error: Failed to decode subroutine type reference: {error:?}"
                        )));
                    }
                }
            }
            gimli::DW_TAG_compile_unit => {
                // This only happens when we do a 'lazy' load of [VariableName::StaticScope]
                child_variable =
                    self.process_tree(debug_info, node, child_variable, memory, cache, frame_info)?;
            }

            other @ (gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type) => match node.entry().attr(gimli::DW_AT_type) {
                Ok(Some(attr)) => {
                    child_variable = self.process_type_attribute(
                        &attr,
                        debug_info,
                        node.entry(),
                        parent_variable,
                        child_variable,
                        memory,
                        frame_info,
                        cache,
                    )?
                }

                Ok(None) => child_variable.set_value(self.language.process_tag_with_no_type(other)),

                Err(error) => child_variable.set_value(VariableValue::Error(format!(
                    "Error: Failed to decode {other:?} type reference: {error:?}"
                ))),
            },

            // Do not expand this type.
            other => {
                child_variable.set_value(VariableValue::Error(format!(
                    "<unimplemented: type: {:?}>",
                    other.static_string()
                )));
                child_variable.type_name = VariableType::Other("unimplemented".to_string());
                cache.remove_cache_entry_children(child_variable.variable_key)?;
            }
        }

        cache.update_variable_and_value(&mut child_variable, memory)?;

        Ok(child_variable)
    }

    /// Create child variable entries to represent array members and their values.
    #[allow(clippy::too_many_arguments)]
    fn expand_array_member(
        &self,
        debug_info: &DebugInfo,
        unit_ref: UnitOffset,
        cache: &mut VariableCache,
        child_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        array_member_index: i64,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let mut array_member_type_tree = self
            .unit
            .header
            .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;

        let Ok(array_member_type_node) = array_member_type_tree.root() else {
            return Ok(());
        };
        let mut array_member_variable =
            cache.create_variable(child_variable.variable_key, Some(unit_ref), Some(self))?;
        array_member_variable.member_index = Some(array_member_index);
        // Override the calculated member name with a more 'array-like' name.
        array_member_variable.name = VariableName::Named(format!("__{array_member_index}"));
        array_member_variable.source_location = child_variable.source_location.clone();
        self.process_memory_location(
            debug_info,
            array_member_type_node.entry(),
            child_variable,
            &mut array_member_variable,
            memory,
            frame_info,
        )?;
        array_member_variable = self.extract_type(
            debug_info,
            array_member_type_node,
            child_variable,
            array_member_variable,
            memory,
            cache,
            frame_info,
        )?;
        if array_member_index == child_variable.range_lower_bound {
            // Once we know the type of the first member, we can set the array type.
            child_variable.type_name = VariableType::Array {
                count: child_variable.range_upper_bound as usize,
                item_type_name: array_member_variable.type_name.to_string(),
            };
            // Once we know the byte_size of the first member, we can set the array byte_size.
            if let Some(array_member_byte_size) = array_member_variable.byte_size {
                child_variable.byte_size =
                    Some(array_member_byte_size * child_variable.subrange_bounds().count() as u64);
            }
            // Make sure the array variable has no value if its own.
            child_variable.set_value(VariableValue::Empty);
        }
        self.handle_memory_location_special_cases(
            unit_ref,
            &mut array_member_variable,
            child_variable,
            memory,
        );
        cache.update_variable_and_value(&mut array_member_variable, memory)?;

        Ok(())
    }

    /// Process a memory location for a variable, by first evaluating the `byte_size`, and then calling the `self.extract_location`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process_memory_location(
        &self,
        debug_info: &DebugInfo,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        // The `byte_size` is used for arrays, etc. to offset the memory location of the next element.
        // For nested arrays, the `byte_size` may need to be calculated as the product of the `byte_size` and array upper bound.
        child_variable.byte_size = child_variable
            .byte_size
            .or_else(|| extract_byte_size(node_die))
            .or_else(|| {
                if let VariableType::Array { .. } = parent_variable.type_name {
                    parent_variable.byte_size.map(|byte_size| {
                        let array_member_count = parent_variable.subrange_bounds().count() as u64;
                        if array_member_count > 0 {
                            byte_size / array_member_count
                        } else {
                            byte_size
                        }
                    })
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
                    tracing::debug!("Encountered an unsupported location expression while resolving the location for variable {variable_name:?}: {debug_error:?}. Please reduce optimization levels in your build profile.");
                    return Ok(());
                }
            };

            match expression_result {
                ExpressionResult::Value(value_from_expression @ VariableValue::Valid(_)) => {
                    // The ELF contained the actual value, not just a location to it.
                    child_variable.memory_location = VariableLocation::Value;
                    child_variable.set_value(value_from_expression);
                }

                ExpressionResult::Value(value_from_expression) => {
                    child_variable.set_value(value_from_expression);
                }

                ExpressionResult::Location(VariableLocation::Unavailable) => {
                    child_variable.set_value(VariableValue::Error(
                        "<value optimized away by compiler, out of scope, or dropped>".to_string(),
                    ));
                }

                ExpressionResult::Location(
                    VariableLocation::Error(error_message)
                    | VariableLocation::Unsupported(error_message),
                ) => {
                    child_variable.set_value(VariableValue::Error(error_message.clone()));
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
    /// Return values are implemented as follows:
    /// - Result<_, DebugError>: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - Result<ExpressionResult::Value(),_>:  The value is statically stored in the binary, and can be returned, and has no relevant memory location.
    /// - Result<ExpressionResult::Location(),_>:  One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn extract_location(
        &self,
        debug_info: &DebugInfo,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_location: &VariableLocation,
        memory: &mut dyn MemoryInterface,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<ExpressionResult, DebugError> {
        trait ResultExt {
            /// Turns UnwindIncompleteResults into Unavailable locations
            fn convert_incomplete(self) -> Result<ExpressionResult, DebugError>;
        }

        impl ResultExt for Result<ExpressionResult, DebugError> {
            fn convert_incomplete(self) -> Result<ExpressionResult, DebugError> {
                match self {
                    Ok(result) => Ok(result),
                    Err(DebugError::UnwindIncompleteResults { message }) => {
                        tracing::warn!("UnwindIncompleteResults: {:?}", message);
                        Ok(ExpressionResult::Location(VariableLocation::Unavailable))
                    }
                    e => e,
                }
            }
        }

        let mut attrs = node_die.attrs();
        while let Ok(Some(attr)) = attrs.next() {
            let result = match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_frame_base
                | gimli::DW_AT_data_member_location => match attr.value() {
                    gimli::AttributeValue::Exprloc(expression) => self
                        .evaluate_expression(memory, expression, frame_info)
                        .convert_incomplete()?,

                    gimli::AttributeValue::Udata(offset_from_location) => {
                        let location = if let VariableLocation::Address(address) = parent_location {
                            let Some(location) = address.checked_add(offset_from_location) else {
                                return Err(DebugError::UnwindIncompleteResults {
                                    message: "Overflow calculating variable address"
                                        .to_string(),
                                });
                            };

                            VariableLocation::Address(location)
                        } else {
                            parent_location.clone()
                        };

                        ExpressionResult::Location(location)
                    }

                    gimli::AttributeValue::LocationListsRef(location_list_offset) => self
                        .evaluate_location_list_ref(
                            debug_info,
                            location_list_offset,
                            frame_info,
                            memory,
                        )
                        .convert_incomplete()?,

                    other_attribute_value => {
                        ExpressionResult::Location(VariableLocation::Unsupported(format!(
                            "Unimplemented: extract_location() Could not extract location from: {:.100}",
                            format!("{other_attribute_value:?}")
                        )))
                    }
                },

                gimli::DW_AT_address_class => {
                    let location = match attr.value() {
                        gimli::AttributeValue::AddressClass(gimli::DwAddr(0)) => {
                            // We pass on the location of the parent, which will later to be used along with DW_AT_data_member_location to calculate the location of this variable.
                            parent_location.clone()
                        }
                        gimli::AttributeValue::AddressClass(address_class) => {
                            VariableLocation::Unsupported(format!(
                                "Unimplemented: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({address_class:?}))"
                            ))
                        }
                        other_attribute_value => {
                            VariableLocation::Unsupported(format!(
                                "Unimplemented: extract_location() found invalid DW_AT_address_class: {:.100}",
                                format!("{other_attribute_value:?}")
                            ))
                        }
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
        frame_info: StackFrameInfo<'_>,
        memory: &mut dyn MemoryInterface,
    ) -> Result<ExpressionResult, DebugError> {
        let mut locations = match debug_info.locations_section.locations(
            location_list_offset,
            self.unit.header.encoding(),
            self.unit.low_pc,
            &debug_info.address_section,
            self.unit.addr_base,
        ) {
            Ok(locations) => locations,
            Err(error) => {
                return Ok(ExpressionResult::Location(VariableLocation::Error(
                    format!("Error: Resolving variable Location: {:?}", error),
                )))
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

            if let Ok(program_counter) = program_counter.try_into() {
                if location.range.contains(program_counter) {
                    expression = Some(location.data);
                    break 'find_range;
                }
            }
        }

        let Some(valid_expression) = expression else {
            return Ok(ExpressionResult::Location(VariableLocation::Unavailable));
        };

        self.evaluate_expression(memory, valid_expression, frame_info)
    }

    /// Evaluate a [`gimli::Expression`] as a valid memory location.
    /// Return values are implemented as follows:
    /// - `Result<_, DebugError>`: This happens when we encounter an error we did not expect, and will propagate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - `Result<ExpressionResult::Value(),_>`: The value is statically stored in the binary, and can be returned, and has no relevant memory location.
    /// - `Result<ExpressionResult::Location(),_>`: One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn evaluate_expression(
        &self,
        memory: &mut dyn MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<ExpressionResult, DebugError> {
        fn evaluate_address(address: u64, memory: &mut dyn MemoryInterface) -> ExpressionResult {
            let location = if address >= u32::MAX as u64 && !memory.supports_native_64bit_access() {
                VariableLocation::Error(format!("The memory location for this variable value ({:#010X}) is invalid. Please report this as a bug.", address))
            } else {
                VariableLocation::Address(address)
            };

            ExpressionResult::Location(location)
        }

        let pieces = self.expression_to_piece(memory, expression, frame_info)?;

        if pieces.is_empty() {
            return Ok(ExpressionResult::Location(VariableLocation::Error(
                "Error: expr_to_piece() returned 0 results".to_string(),
            )));
        }
        if pieces.len() > 1 {
            return Ok(ExpressionResult::Location(VariableLocation::Error(
                "<unsupported memory implementation>".to_string(),
            )));
        }

        let result = match &pieces[0].location {
            Location::Empty => {
                // This means the value was optimized away.
                ExpressionResult::Location(VariableLocation::Unavailable)
            }
            Location::Address { address } if address.is_zero() => {
                let error = "The value of this variable may have been optimized out of the debug info, by the compiler.".to_string();
                ExpressionResult::Location(VariableLocation::Error(error))
            }
            Location::Address { address } => evaluate_address(*address, memory),
            Location::Value { value } => {
                let value = match value {
                    gimli::Value::Generic(value) => value.to_string(),
                    gimli::Value::I8(value) => value.to_string(),
                    gimli::Value::U8(value) => value.to_string(),
                    gimli::Value::I16(value) => value.to_string(),
                    gimli::Value::U16(value) => value.to_string(),
                    gimli::Value::I32(value) => value.to_string(),
                    gimli::Value::U32(value) => value.to_string(),
                    gimli::Value::I64(value) => value.to_string(),
                    gimli::Value::U64(value) => value.to_string(),
                    gimli::Value::F32(value) => value.to_string(),
                    gimli::Value::F64(value) => value.to_string(),
                };

                ExpressionResult::Value(VariableValue::Valid(value))
            }
            Location::Register { register } => {
                if let Some(address) = frame_info
                    .registers
                    .get_register_by_dwarf_id(register.0)
                    .and_then(|register| register.value)
                {
                    match address.try_into() {
                        Ok(address) => evaluate_address(address, memory),
                        Err(error) => ExpressionResult::Location(VariableLocation::Error(format!(
                            "Error: Cannot convert register value to location address: {error:?}"
                        ))),
                    }
                } else {
                    ExpressionResult::Location(VariableLocation::Error(format!(
                        "Error: Cannot resolve register: {register:?}"
                    )))
                }
            }
            l => ExpressionResult::Location(VariableLocation::Error(format!(
                "Unimplemented: extract_location() found a location type: {:.100}",
                format!("{l:?}")
            ))),
        };

        Ok(result)
    }

    /// Tries to get the result of a DWARF expression in the form of a Piece.
    pub(crate) fn expression_to_piece(
        &self,
        memory: &mut dyn MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        frame_info: StackFrameInfo<'_>,
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
                unimplemented_expression => {
                    return Err(DebugError::UnwindIncompleteResults {
                        message: format!("Unimplemented: Expressions that include {unimplemented_expression:?} are not currently supported."
                    )});
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
        let location = if let Some(child_member_index) = child_variable.member_index {
            // If this variable is a member of an array type, and needs special handling to calculate the `memory_location`.
            if let VariableLocation::Address(address) = parent_variable.memory_location {
                if let Some(byte_size) = child_variable.byte_size {
                    let Some(location) = address.checked_add(child_member_index as u64 * byte_size)
                    else {
                        child_variable.set_value(VariableValue::Error(
                            "Overflow calculating variable address".to_string(),
                        ));
                        return;
                    };

                    VariableLocation::Address(location)
                } else {
                    // If this array member doesn't have a byte_size, it may be because it is the first member of an array itself.
                    // In this case, the byte_size will be calculated when the nested array members are resolved.
                    // The first member of an array will have a memory location of the same as it's parent.
                    parent_variable.memory_location.clone()
                }
            } else {
                VariableLocation::Unavailable
            }
        } else if child_variable.memory_location == VariableLocation::Unknown {
            // Non-array members can inherit their memory location from their parent, but only if the parent has a valid memory location.
            if self.is_pointer(child_variable, parent_variable, unit_ref) {
                match &parent_variable.memory_location {
                    VariableLocation::Address(address) => {
                        // Now, retrieve the location by reading the adddress pointed to by the parent variable.
                        match memory.read_word_32(*address) {
                            Ok(memory_location) => {
                                VariableLocation::Address(memory_location as u64)
                            }
                            Err(error) => {
                                tracing::debug!("Failed to read referenced variable address from memory location {} : {error}.", parent_variable.memory_location);
                                VariableLocation::Error(format!("Failed to read referenced variable address from memory location {} : {error}.", parent_variable.memory_location))
                            }
                        }
                    }
                    other => VariableLocation::Unsupported(format!(
                        "Location {other:?} not supported for referenced variables."
                    )),
                }
            } else {
                // If the parent variable is not a pointer, or it is a pointer to the actual data location
                // (not the address of the data location) then it can inherit it's memory location from it's parent.
                parent_variable.memory_location.clone()
            }
        } else {
            return;
        };

        child_variable.memory_location = location;
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
        // 5. Pointers to types with refrenced memory addresses (e.g. variants, generics, arrays, etc.)
        (matches!(child_variable.name.clone(), VariableName::Named(var_name) if var_name.starts_with('*'))
                && matches!(parent_variable.role, VariantRole::VariantPart(_)))
            || matches!(&parent_variable.type_name, VariableType::Pointer(Some(pointer_name)) if pointer_name.starts_with('*'))
            || (matches!(&parent_variable.type_name, VariableType::Pointer(_))
                && (matches!(child_variable.type_name, VariableType::Base(_))
                    || matches!(child_variable.type_name.clone(), VariableType::Struct(type_name) if type_name.starts_with("&str"))
                    || matches!(child_variable.name.clone(), VariableName::Named(var_name) if var_name.starts_with('*'))
                    || self.has_address_pointer(unit_ref).unwrap_or_else(|error| {
                        child_variable.set_value(VariableValue::Error(format!("Failed to determine if a struct has variant or generic type fields: {error}")));
                        false
                    })))
    }

    /// A helper function to determine if the type we are referencing requires a pointer to the address of the referenced variable (e.g. variants, generics, arrays, etc.)
    fn has_address_pointer(&self, unit_ref: UnitOffset) -> Result<bool, DebugError> {
        let mut entries_tree = self
            .unit
            .header
            .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
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
}

fn extract_name(
    debug_info: &DebugInfo,
    entry: &gimli::DebuggingInformationEntry<GimliReader>,
) -> Result<Option<String>, gimli::Error> {
    let attr = match entry.attr(gimli::DW_AT_name) {
        Ok(Some(attr)) => attr.value(),
        Ok(None) => return Ok(None),
        Err(error) => return Err(error),
    };

    let name = match attr {
        gimli::AttributeValue::DebugStrRef(name_ref) => {
            if let Ok(name_raw) = debug_info.dwarf.string(name_ref) {
                String::from_utf8_lossy(&name_raw).to_string()
            } else {
                "Invalid DW_AT_name value".to_string()
            }
        }
        gimli::AttributeValue::String(name) => String::from_utf8_lossy(&name).to_string(),
        other => format!("Unimplemented: Evaluate name from {other:?}"),
    };

    Ok(Some(name))
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
        Some(_) => Err(DebugError::UnwindIncompleteResults {
            message: format!(
                "Unimplemented: Support for type {:?} in `RequiresRegister`",
                base_type
            ),
        }),
        None => Err(DebugError::UnwindIncompleteResults {
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
        return Err(DebugError::UnwindIncompleteResults {
            message: "Cannot unwind `Variable` location without a valid frame base address.)"
                .to_string(),
        });
    };
    match evaluation.resume_with_frame_base(frame_base) {
        Ok(evaluation_result) => Ok(evaluation_result),
        Err(error) => Err(DebugError::UnwindIncompleteResults {
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
        return Err(DebugError::UnwindIncompleteResults {
            message: "Cannot unwind `Variable` location without a valid canonical frame address.)"
                .to_string(),
        });
    };
    match evaluation.resume_with_call_frame_cfa(cfa) {
        Ok(evaluation_result) => Ok(evaluation_result),
        Err(error) => Err(DebugError::UnwindIncompleteResults {
            message: format!("Error while calculating `Variable::memory_location`:{error}."),
        }),
    }
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
            DebugError::UnwindIncompleteResults {
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
        x => {
            return Err(DebugError::UnwindIncompleteResults {
                message: format!(
                    "Unimplemented: Requested memory with size {x}, which is not supported yet."
                ),
            });
        }
    };

    Ok(evaluation.resume_with_memory(val)?)
}

trait RangeExt {
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
