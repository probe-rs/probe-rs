use super::{
    debug_info::*, extract_byte_size, extract_file, extract_line, extract_name,
    function_die::FunctionDie, registers, variable::*, DebugError, DebugRegisters, EndianReader,
    SourceLocation, VariableCache,
};
use crate::{core::RegisterValue, Error, MemoryInterface};
use gimli::{AttributeValue::Language, EvaluationResult, Location, UnitOffset};
use num_traits::Zero;

pub(crate) type UnitIter =
    gimli::DebugInfoUnitHeadersIter<gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>>;

/// The result of `UnitInfo::evaluate_expression()` can be the value of a variable, or a memory location.
pub(crate) enum ExpressionResult {
    Value(VariableValue),
    Location(VariableLocation),
}

/// A struct containing information about a single compilation unit.
pub(crate) struct UnitInfo<'debuginfo> {
    pub(crate) debug_info: &'debuginfo DebugInfo,
    pub(crate) unit: gimli::Unit<GimliReader, usize>,
}

impl<'debuginfo> UnitInfo<'debuginfo> {
    /// Retrieve the value of the `DW_AT_language` attribute of the compilation unit.
    ///
    /// In the unlikely event that we are unable to retrieve the language, we assume Rust.
    pub(crate) fn get_language(&self) -> gimli::DwLang {
        let Ok(Some(Language(unit_language))) = self
            .unit
            .header
            .entries_tree(&self.unit.abbreviations, None)
            .and_then(|mut tree| tree.root()?.entry().attr_value(gimli::DW_AT_language))
        else {
            tracing::warn!("Unable to retrieve DW_AT_language attribute, assuming Rust.");
            return gimli::DW_LANG_Rust;
        };
        unit_language
    }

    /// Get the DIEs for the function containing the given address.
    ///
    /// If `find_inlined` is `false`, then the result will contain a single [`FunctionDie`]
    /// If `find_inlined` is `true`, then the result will contain a  [`Vec<FunctionDie>`], where the innermost (deepest in the stack) function die is the last entry in the Vec.
    pub(crate) fn get_function_dies(
        &self,
        address: u64,
        find_inlined: bool,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        tracing::trace!("Searching Function DIE for address {:#x}", address);

        let mut entries_cursor = self.unit.entries();
        while let Ok(Some((_depth, current))) = entries_cursor.next_dfs() {
            let Some(die) = FunctionDie::new(current.clone(), self) else {
                continue;
            };

            let mut ranges = self.debug_info.dwarf.die_ranges(&self.unit, current)?;

            while let Ok(Some(ranges)) = ranges.next() {
                if !(ranges.begin <= address && address < ranges.end) {
                    continue;
                }
                let mut die = die.clone();
                // Check if we are actually in an inlined function
                die.low_pc = ranges.begin;
                die.high_pc = ranges.end;

                // Extract the frame_base for this function DIE.
                let mut functions = vec![die];

                tracing::debug!("Found DIE: name={:?}", functions[0].function_name());

                if find_inlined {
                    tracing::debug!("Checking for inlined functions");

                    let inlined_functions =
                        self.find_inlined_functions(address, current.offset())?;

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
        address: u64,
        offset: UnitOffset,
    ) -> Result<Vec<FunctionDie>, DebugError> {
        let mut current_depth = 0;
        let mut abort_depth = 0;
        let mut functions = Vec::new();

        // If we don't have any entries at our unit offset, return an empty vector.
        let Ok(mut cursor) = self.unit.entries_at_offset(offset) else {
            return Ok(vec![]);
        };
        while let Ok(Some((depth, current))) = cursor.next_dfs() {
            current_depth += depth;
            if current_depth < abort_depth {
                break;
            }

            // Skip anything that is not an inlined subroutine.
            if current.tag() != gimli::DW_TAG_inlined_subroutine {
                continue;
            }

            let mut ranges = self.debug_info.dwarf.die_ranges(&self.unit, current)?;

            while let Ok(Some(ranges)) = ranges.next() {
                if !(ranges.begin <= address && address < ranges.end) {
                    continue;
                };
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

                die.low_pc = ranges.begin;
                die.high_pc = ranges.end;

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
        tree_node: &mut gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &mut Variable,
        mut child_variable: Variable,
        memory: &mut impl MemoryInterface,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
        cache: &mut VariableCache,
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
                        tree_node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
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
        if let Some(name) = attributes_entry
            .as_ref()
            .map(|ae| ae.attr_value(gimli::DW_AT_name))
            .transpose()?
            .flatten()
        {
            child_variable.name = VariableName::Named(extract_name(self.debug_info, name));
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
                            extract_file(self.debug_info, &self.unit, attr.value())
                        {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: existing_source_location.line,
                                        column: existing_source_location.column,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: None,
                                        column: None,
                                        file: Some(file_name),
                                        directory: Some(directory),
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                            }
                        };
                    }
                    gimli::DW_AT_decl_line => {
                        if let Some(line_number) = extract_line(attr.value()) {
                            match child_variable.source_location {
                                Some(existing_source_location) => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: existing_source_location.column,
                                        file: existing_source_location.file,
                                        directory: existing_source_location.directory,
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                                None => {
                                    child_variable.source_location = Some(SourceLocation {
                                        line: Some(line_number),
                                        column: None,
                                        file: None,
                                        directory: None,
                                        low_pc: None,
                                        high_pc: None,
                                    });
                                }
                            }
                        };
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
                        match attr.value() {
                            gimli::AttributeValue::UnitRef(unit_ref) => {
                                // Reference to a type, or an entry to another type or a type modifier which will point to another type.
                                // Before we resolve that type tree, we need to resolve the current node's memory location.
                                // This is because the memory location of the type nodes and child variables often inherit this value.
                                self.process_memory_location(
                                    &attributes_entry,
                                    parent_variable,
                                    &mut child_variable,
                                    memory,
                                    stack_frame_registers,
                                    frame_base,
                                )?;

                                // Now reslolve the referenced tree node for the type.
                                let mut type_tree = self
                                    .unit
                                    .header
                                    .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
                                let referenced_type_tree_node = type_tree.root()?;
                                child_variable = self.extract_type(
                                    referenced_type_tree_node,
                                    parent_variable,
                                    child_variable,
                                    memory,
                                    stack_frame_registers,
                                    frame_base,
                                    cache,
                                )?;
                            }
                            other_attribute_value => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_type {other_attribute_value:?}"
                                )));
                            }
                        }
                    }
                    gimli::DW_AT_enum_class => match attr.value() {
                        gimli::AttributeValue::Flag(is_enum_class) => {
                            if is_enum_class {
                                child_variable.set_value(VariableValue::Valid(format!(
                                    "{:?}",
                                    child_variable.type_name.clone()
                                )));
                            } else {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Flag Value for DW_AT_enum_class {is_enum_class:?}"
                                )));
                            }
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_enum_class: {other_attribute_value:?}"
                            )));
                        }
                    },
                    gimli::DW_AT_const_value => match attr.value() {
                        gimli::AttributeValue::Udata(const_value) => {
                            child_variable.set_value(VariableValue::Valid(const_value.to_string()));
                        }
                        other_attribute_value => {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unimplemented: Attribute Value for DW_AT_const_value: {other_attribute_value:?}"
                            )));
                        }
                    },
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
                            let mut discriminant_variable = cache.cache_variable(
                                parent_variable.variable_key,
                                Variable::new(
                                    self.unit.header.offset().as_debug_info_offset(),
                                    Some(discriminant_node.entry().offset()),
                                ),
                                memory,
                            )?;
                            discriminant_variable = self.process_tree_node_attributes(
                                &mut discriminant_node,
                                parent_variable,
                                discriminant_variable,
                                memory,
                                stack_frame_registers,
                                frame_base,
                                cache,
                            )?;
                            if !discriminant_variable.is_valid() {
                                parent_variable.role = VariantRole::VariantPart(u64::MAX);
                            } else {
                                parent_variable.role = VariantRole::VariantPart(
                                    discriminant_variable
                                        .get_value(cache)
                                        .parse()
                                        .unwrap_or(u64::MAX),
                                );
                            }
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
                        Some(lower_bound) => child_variable.range_lower_bound = lower_bound as i64,
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
                            Some(upper_bound) => {
                                child_variable.range_upper_bound = upper_bound as i64
                            }
                            None => {
                                child_variable.set_value(VariableValue::Error(format!(
                                    "Unimplemented: Attribute Value for DW_AT_upper_bound: {:?}",
                                    attr.value()
                                )));
                            }
                        }
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
                        #[allow(clippy::format_in_format_args)]
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
        cache
            .cache_variable(child_variable.parent_key, child_variable, memory)
            .map_err(|error| error.into())
    }

    /// Recurse the ELF structure below the `parent_node`, and ...
    /// - Consumes the `parent_variable`.
    /// - Updates the `DebugInfo::VariableCache` with all descendant `Variable`s.
    /// - Returns a clone of the most up-to-date `parent_variable` in the cache.
    pub(crate) fn process_tree(
        &self,
        parent_node: gimli::EntriesTreeNode<GimliReader>,
        mut parent_variable: Variable,
        memory: &mut impl MemoryInterface,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        if parent_variable.is_valid() {
            let program_counter = if let Some(program_counter) = stack_frame_registers
                .get_program_counter()
                .and_then(|reg| reg.value)
            {
                program_counter.try_into()?
            } else {
                return Err(DebugError::UnwindIncompleteResults {
                    message: "Cannot unwind `Variable` without a valid PC (program_counter)"
                        .to_string(),
                });
            };

            tracing::trace!("process_tree for parent {:?}", parent_variable.variable_key);

            let mut child_nodes = parent_node.children();
            while let Some(mut child_node) = child_nodes.next()? {
                match child_node.entry().tag() {
                    gimli::DW_TAG_namespace => {
                        // Use these parents to extract `statics`.
                        let mut namespace_variable = Variable::new(
                            self.unit.header.offset().as_debug_info_offset(),
                            Some(child_node.entry().offset()),
                        );

                        namespace_variable.name = if let Ok(Some(attr)) = child_node.entry().attr(gimli::DW_AT_name) {
                            VariableName::Namespace(extract_name(self.debug_info, attr.value()))
                        } else { VariableName::AnonymousNamespace };
                        namespace_variable.type_name = VariableType::Namespace;
                        namespace_variable.memory_location = VariableLocation::Unavailable;
                        namespace_variable = cache.cache_variable(parent_variable.variable_key, namespace_variable, memory)?;

                        let mut namespace_children_nodes = child_node.children();
                        while let Some(mut namespace_child_node) = namespace_children_nodes.next()? {
                            match namespace_child_node.entry().tag() {
                                gimli::DW_TAG_variable => {
                                    // We only want the TOP level variables of the namespace (statics).
                                    let static_child_variable = cache.cache_variable(namespace_variable.variable_key, Variable::new(
                                        self.unit.header.offset().as_debug_info_offset(),
                                        Some(namespace_child_node.entry().offset()),), memory)?;
                                    self.process_tree_node_attributes(&mut namespace_child_node, &mut namespace_variable, static_child_variable, memory, stack_frame_registers, frame_base, cache)?;
                                }
                                gimli::DW_TAG_namespace => {
                                    // Recurse for additional namespace variables.
                                    let mut namespace_child_variable = Variable::new(
                                        self.unit.header.offset().as_debug_info_offset(),
                                        Some(namespace_child_node.entry().offset()),);
                                    namespace_child_variable.name = if let Ok(Some(attr)) = namespace_child_node.entry().attr(gimli::DW_AT_name) {

                                        match &namespace_variable.name {
                                            VariableName::Namespace(name) => {
                                            VariableName::Namespace(format!("{}::{}", name, extract_name(self.debug_info, attr.value())))
                                            }
                                            other => return Err(DebugError::UnwindIncompleteResults {message: format!("Unable to construct namespace variable, unexpected parent name: {other:?}")})
                                        }

                                    } else { VariableName::AnonymousNamespace};
                                    namespace_child_variable.type_name = VariableType::Namespace;
                                    namespace_child_variable.memory_location = VariableLocation::Unavailable;
                                    namespace_child_variable = cache.cache_variable(namespace_variable.variable_key, namespace_child_variable, memory)?;
                                    namespace_child_variable = self.process_tree(namespace_child_node, namespace_child_variable, memory, stack_frame_registers, frame_base, cache, )?;
                                    if !cache.has_children(&namespace_child_variable)? {
                                        cache.remove_cache_entry(namespace_child_variable.variable_key)?;
                                    }
                                }
                                _ => {
                                    // We only want namespace and variable children.
                                }
                            }
                        }
                        if !cache.has_children(&namespace_variable)? {
                            cache.remove_cache_entry(namespace_variable.variable_key)?;
                        }
                    }
                    gimli::DW_TAG_formal_parameter | // Parameters to functions.
                    gimli::DW_TAG_variable         | // Typical top-level variables.
                    gimli::DW_TAG_member           | // Members of structured types.
                    gimli::DW_TAG_enumerator         // Possible values for enumerators, used by extract_type() when processing DW_TAG_enumeration_type.
                    => {
                        let mut child_variable = cache.cache_variable(parent_variable.variable_key, Variable::new(
                        self.unit.header.offset().as_debug_info_offset(),
                        Some(child_node.entry().offset()),
                    ), memory)?;
                        child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, memory, stack_frame_registers, frame_base, cache,)?;
                        // Do not keep or process PhantomData nodes, or variant parts that we have already used.
                        if child_variable.type_name.is_phantom_data()
                            ||  child_variable.name == VariableName::Artifical
                        {
                            cache.remove_cache_entry(child_variable.variable_key)?;
                        } else if child_variable.is_valid() {
                            // Recursively process each child.
                            self.process_tree(child_node, child_variable, memory, stack_frame_registers, frame_base, cache, )?;
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
                        let mut child_variable = cache.cache_variable(
                            parent_variable.variable_key,
                            Variable::new(self.unit.header.offset().as_debug_info_offset(),Some(child_node.entry().offset())),
                            memory
                        )?;
                        // To determine the discriminant, we use the following rules:
                        // - If there is no DW_AT_discr, then there will be a single DW_TAG_variant, and this will be the matching value. In the code here, we assign a default value of u64::MAX to both, so that they will be matched as belonging together (https://dwarfstd.org/ShowIssue.php?issue=180517.2)
                        // - TODO: The [DWARF] standard, 5.7.10, allows for a case where there is no DW_AT_discr attribute, but a DW_AT_type to represent the tag. I have not seen that generated from RUST yet.
                        // - If there is a DW_AT_discr that has a value, then this is a reference to the member entry for the discriminant. This value will be resolved to match against the appropriate DW_TAG_variant.
                        // - TODO: The [DWARF] standard, 5.7.10, allows for a DW_AT_discr_list, but I have not seen that generated from RUST yet. 
                        parent_variable.role = VariantRole::VariantPart(u64::MAX);
                        child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, memory, stack_frame_registers, frame_base, cache, )?;
                        // At this point we have everything we need (It has updated the parent's `role`) from the child_variable, so elimnate it before we continue ...
                        cache.remove_cache_entry(child_variable.variable_key)?;
                        parent_variable = self.process_tree(child_node, parent_variable, memory, stack_frame_registers, frame_base, cache)?;
                    }
                    gimli::DW_TAG_variant // variant is a child of a structure, and one of them should have a discriminant value to match the DW_TAG_variant_part 
                    => {
                        // We only need to do this if we have not already found our variant,
                        if !cache.has_children(&parent_variable)? {
                            let mut child_variable = cache.cache_variable(
                                parent_variable.variable_key,
                                Variable::new(self.unit.header.offset().as_debug_info_offset(), Some(child_node.entry().offset())),
                                memory
                            )?;
                            self.extract_variant_discriminant(&child_node, &mut child_variable)?;
                            child_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, child_variable, memory, stack_frame_registers, frame_base, cache)?;
                            if child_variable.is_valid() {
                                if let VariantRole::Variant(discriminant) = child_variable.role {
                                    // Only process the discriminant variants or when we eventually   encounter the default 
                                    if parent_variable.role == VariantRole::VariantPart(discriminant) || discriminant == u64::MAX {
                                        self.process_memory_location(child_node.entry(), &parent_variable, &mut child_variable, memory, stack_frame_registers, frame_base)?;
                                        // Recursively process each relevant child node.
                                        child_variable = self.process_tree(child_node, child_variable, memory, stack_frame_registers, frame_base, cache)?;
                                        if child_variable.is_valid() {
                                            // Eliminate intermediate DWARF nodes, but keep their children
                                            cache.adopt_grand_children(&parent_variable, &child_variable)?;
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
                        let mut range_variable = cache.cache_variable(parent_variable.variable_key,Variable::new(
                        self.unit.header.offset().as_debug_info_offset(),
                        Some(child_node.entry().offset()),
                    ), memory)?;
                        range_variable = self.process_tree_node_attributes(&mut child_node, &mut parent_variable, range_variable, memory, stack_frame_registers, frame_base, cache)?;
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
                        let mut in_scope =  false;
                        if let Ok(Some(low_pc_attr)) = child_node.entry().attr(gimli::DW_AT_low_pc) {
                            let low_pc = match low_pc_attr.value() {
                                gimli::AttributeValue::Addr(value) => value,
                                _other => u64::MAX,
                            };
                            let high_pc = if let Ok(Some(high_pc_attr))
                                = child_node.entry().attr(gimli::DW_AT_high_pc) {
                                    match high_pc_attr.value() {
                                        gimli::AttributeValue::Addr(addr) => addr,
                                        gimli::AttributeValue::Udata(unsigned_offset) => low_pc + unsigned_offset,
                                        _other => 0_u64,
                                    }
                            } else { 0_u64};
                            if low_pc == u64::MAX || high_pc == 0_u64 {
                                // These have not been specified correctly ... something went wrong.
                                parent_variable.set_value(VariableValue::Error("Error: Processing of variables failed because of invalid/unsupported scope information. Please log a bug at 'https://github.com/probe-rs/probe-rs/issues'".to_string()));
                            }
                            if low_pc <= program_counter && program_counter  < high_pc {
                                // We have established positive scope, so no need to continue.
                                in_scope = true;
                            };
                            // No scope info yet, so keep looking. 
                        };
                        // Searching for ranges has a bit more overhead, so ONLY do this if do not have scope confirmed yet.
                        if !in_scope {
                            if let Ok(Some(ranges))
                                = child_node.entry().attr(gimli::DW_AT_ranges) {
                                    match ranges.value() {
                                        gimli::AttributeValue::RangeListsRef(raw_range_lists_offset) => {
                                            let range_lists_offset = self.debug_info.dwarf.ranges_offset_from_raw(&self.unit, raw_range_lists_offset);

                                            if let Ok(mut ranges) = self
                                                .debug_info
                                                .dwarf
                                                .ranges(&self.unit, range_lists_offset) {
                                                    while let Ok(Some(ranges)) = ranges.next() {
                                                        // We have established positive scope, so no need to continue.
                                                        if ranges.begin <= program_counter && program_counter < ranges.end {
                                                            in_scope = true;
                                                            break;
                                                        }
                                                    }
                                                }
                                            }
                                        other_range_attribute => {
                                            parent_variable.set_value(VariableValue::Error(format!("Found unexpected scope attribute: {:?} for variable {:?}", other_range_attribute, parent_variable.name)));
                                        }
                                    }
                            }
                        }
                        if in_scope {
                            // This is IN scope.
                            // Recursively process each child, but pass the parent_variable, so that we don't create intermediate nodes for scope identifiers.
                            parent_variable = self.process_tree(child_node, parent_variable, memory, stack_frame_registers, frame_base, cache)?;
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
                    other => {
                        // One of two things are true here. Either we've encountered a DwTag that is implemented in `extract_type`, and whould be ignored, or we have encountered an unimplemented  DwTag.
                        match other {
                            gimli::DW_TAG_inlined_subroutine | // Inlined subroutines are handled at the [StackFame] level
                            gimli::DW_TAG_base_type |
                            gimli::DW_TAG_pointer_type |
                            gimli::DW_TAG_structure_type |
                            gimli::DW_TAG_enumeration_type |
                            gimli::DW_TAG_array_type |
                            gimli::DW_TAG_subroutine_type |
                            gimli::DW_TAG_subprogram |
                            gimli::DW_TAG_union_type => {
                                // These will be processed elsewhere, or not at all, until we discover a use case that needs to be implemented.
                            }
                            unimplemented => {
                                parent_variable.set_value(VariableValue::Error(format!("Unimplemented: Encountered unimplemented DwTag {:?} for Variable {:?}", unimplemented.static_string(), parent_variable)));
                            }
                        }
                    }
                }
            }
        }
        cache
            .cache_variable(parent_variable.parent_key, parent_variable, memory)
            .map_err(|error| error.into())
    }

    /// Compute the discriminant value of a DW_TAG_variant variable. If it is not explicitly captured in the DWARF, then it is the default value.
    pub(crate) fn extract_variant_discriminant(
        &self,
        node: &gimli::EntriesTreeNode<GimliReader>,
        variable: &mut Variable,
    ) -> Result<(), DebugError> {
        if node.entry().tag() == gimli::DW_TAG_variant {
            variable.role = match node.entry().attr(gimli::DW_AT_discr_value) {
                Ok(optional_discr_value_attr) => {
                    match optional_discr_value_attr {
                        Some(discr_attr) => {
                            match discr_attr.value() {
                                gimli::AttributeValue::Data1(const_value) => {
                                    VariantRole::Variant(const_value as u64)
                                }
                                other_attribute_value => {
                                    variable.set_value(VariableValue::Error(format!("Unimplemented: Attribute Value for DW_AT_discr_value: {:.100}", format!("{other_attribute_value:?}"))));
                                    VariantRole::Variant(u64::MAX)
                                }
                            }
                        }
                        None => {
                            // In the case where the variable is a DW_TAG_variant, but has NO DW_AT_discr_value, then this is the "default" to be used.
                            VariantRole::Variant(u64::MAX)
                        }
                    }
                }
                Err(_error) => {
                    variable.set_value(VariableValue::Error(format!(
                        "Error: Retrieving DW_AT_discr_value for variable {variable:?}"
                    )));
                    VariantRole::NonVariant
                }
            };
        }
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
        node: gimli::EntriesTreeNode<GimliReader>,
        parent_variable: &Variable,
        mut child_variable: Variable,
        memory: &mut impl MemoryInterface,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
        cache: &mut VariableCache,
    ) -> Result<Variable, DebugError> {
        let type_name = match node.entry().attr(gimli::DW_AT_name) {
            Ok(optional_name_attr) => {
                optional_name_attr.map(|name_attr| extract_name(self.debug_info, name_attr.value()))
            }
            Err(error) => {
                let message = format!("Error: evaluating type name: {error:?} ");
                child_variable.set_value(VariableValue::Error(message.clone()));
                Some(message)
            }
        };

        if child_variable.is_valid() {
            match node.entry().tag() {
                gimli::DW_TAG_base_type => {
                    child_variable.type_name =
                        VariableType::Base(type_name.unwrap_or_else(|| "<unnamed>".to_string()));
                    self.process_memory_location(
                        node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                    )?;
                }
                gimli::DW_TAG_pointer_type => {
                    child_variable.type_name = VariableType::Pointer(type_name);
                    self.process_memory_location(
                        node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                    )?;

                    // This needs to resolve the pointer before the regular recursion can continue.
                    match node.entry().attr(gimli::DW_AT_type) {
                        Ok(optional_data_type_attribute) => {
                            match optional_data_type_attribute {
                                Some(data_type_attribute) => {
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
                                                    || optional_name
                                                        .as_ref()
                                                        .unwrap()
                                                        .starts_with("*const")
                                                    || optional_name
                                                        .as_ref()
                                                        .unwrap()
                                                        .starts_with("*mut")
                                                {
                                                    // Resolve the children of this variable, because they contain essential information required to resolve the value
                                                    self.debug_info.cache_deferred_variables(
                                                        cache,
                                                        memory,
                                                        &mut child_variable,
                                                        stack_frame_registers,
                                                        frame_base,
                                                    )?;
                                                } else {
                                                    // This is the case where we defer the processing of child types.
                                                }
                                            } else {
                                                self.debug_info.cache_deferred_variables(
                                                    cache,
                                                    memory,
                                                    &mut child_variable,
                                                    stack_frame_registers,
                                                    frame_base,
                                                )?;
                                            }
                                        }
                                        other_attribute_value => {
                                            child_variable.set_value(VariableValue::Error(
                                                format!(
                                            "Unimplemented: Attribute Value for DW_AT_type {:.100}",
                                            format!("{other_attribute_value:?}")
                                        ),
                                            ));
                                        }
                                    }
                                }
                                None => {
                                    child_variable.set_value(VariableValue::Error(format!(
                                    "Error: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                )));
                                }
                            }
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
                        node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
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
                                child_variable.variable_node_type =
                                    VariableNodeType::RecurseToBaseType;
                                child_variable = self.process_tree(
                                    node,
                                    child_variable,
                                    memory,
                                    stack_frame_registers,
                                    frame_base,
                                    cache,
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
                        node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                    )?;

                    // Recursively process a child types.
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                        cache,
                    )?;
                    if parent_variable.is_valid() && child_variable.is_valid() {
                        let enumerator_values = cache.get_children(child_variable.variable_key)?;

                        if let VariableLocation::Address(address) = child_variable.memory_location {
                            // NOTE: hard-coding value of variable.byte_size to 1 ... replace with code if necessary.
                            let mut buff = [0u8; 1];
                            memory.read(address, &mut buff)?;
                            let this_enum_const_value = u8::from_le_bytes(buff).to_string();
                            let enumumerator_value =
                                match enumerator_values.into_iter().find(|enumerator_variable| {
                                    enumerator_variable.get_value(cache) == this_enum_const_value
                                }) {
                                    Some(this_enum) => this_enum.name,
                                    None => VariableName::Named(
                                        "<Error: Unresolved enum value>".to_string(),
                                    ),
                                };
                            child_variable.set_value(VariableValue::Valid(format!(
                                "{}::{}",
                                child_variable.type_name, enumumerator_value
                            )));
                            // We don't need to keep these children.
                            cache.remove_cache_entry_children(child_variable.variable_key)?;
                        } else {
                            child_variable.set_value(VariableValue::Error(format!(
                                "Unsupported variable location {:?}",
                                child_variable.memory_location
                            )));

                            // We don't need to keep these children.
                            cache.remove_cache_entry_children(child_variable.variable_key)?;
                        }
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
                        Ok(optional_data_type_attribute) => {
                            match optional_data_type_attribute {
                                Some(data_type_attribute) => {
                                    match data_type_attribute.value() {
                                        gimli::AttributeValue::UnitRef(unit_ref) => {
                                            // The memory location of array members build on top of the memory location of the child_variable.
                                            self.process_memory_location(
                                                node.entry(),
                                                parent_variable,
                                                &mut child_variable,
                                                memory,
                                                stack_frame_registers,
                                                frame_base,
                                            )?;
                                            // Now we can explode the array members.
                                            // First get the DW_TAG_subrange child of this node. It has a DW_AT_type that points to DW_TAG_base_type:__ARRAY_SIZE_TYPE__.
                                            let mut subrange_variable = cache.cache_variable(
                                                child_variable.variable_key,
                                                Variable::new(
                                                    self.unit
                                                        .header
                                                        .offset()
                                                        .as_debug_info_offset(),
                                                    Some(node.entry().offset()),
                                                ),
                                                memory,
                                            )?;
                                            subrange_variable = self.process_tree(
                                                node,
                                                subrange_variable,
                                                memory,
                                                stack_frame_registers,
                                                frame_base,
                                                cache,
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
                                                    child_variable.range_lower_bound, child_variable.range_upper_bound)
                                                ));
                                            }
                                            cache.remove_cache_entry(
                                                subrange_variable.variable_key,
                                            )?;

                                            if child_variable.range_upper_bound
                                                - child_variable.range_lower_bound
                                                == 0
                                            {
                                                // Gracefully handle the case where the array is empty.
                                                // - Resolve a 'dummy' child, to determine the type of child_variable.
                                                self.expand_array_member(
                                                    unit_ref,
                                                    cache,
                                                    &mut child_variable,
                                                    memory,
                                                    0,
                                                    stack_frame_registers,
                                                    frame_base,
                                                )?;
                                                // - Delete the dummy child that was created above.
                                                cache.remove_cache_entry_children(
                                                    child_variable.variable_key,
                                                )?;
                                            } else {
                                                // - Next, process this DW_TAG_array_type's DW_AT_type full tree.
                                                // - We have to do this repeatedly, for every array member in the range.
                                                for array_member_index in child_variable
                                                    .range_lower_bound
                                                    ..child_variable.range_upper_bound
                                                {
                                                    self.expand_array_member(
                                                        unit_ref,
                                                        cache,
                                                        &mut child_variable,
                                                        memory,
                                                        array_member_index,
                                                        stack_frame_registers,
                                                        frame_base,
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
                                None => {
                                    child_variable.set_value(VariableValue::Error(format!(
                                    "Error: No Attribute Value for DW_AT_type for variable {:?}",
                                    child_variable.name
                                )));
                                }
                            }
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
                        node.entry(),
                        parent_variable,
                        &mut child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                    )?;
                    // Recursively process a child types.
                    // TODO: The DWARF does not currently hold information that allows decoding of which UNION arm is instantiated, so we have to display all available.
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                        cache,
                    )?;
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
                        Ok(optional_data_type_attribute) => {
                            match optional_data_type_attribute {
                                Some(data_type_attribute) => match data_type_attribute.value() {
                                    gimli::AttributeValue::UnitRef(unit_ref) => {
                                        let subroutine_type_node = self
                                            .unit
                                            .header
                                            .entry(&self.unit.abbreviations, unit_ref)?;
                                        child_variable.type_name = match subroutine_type_node
                                            .attr(gimli::DW_AT_name)
                                        {
                                            Ok(optional_name_attr) => match optional_name_attr {
                                                Some(name_attr) => {
                                                    VariableType::Other(extract_name(
                                                        self.debug_info,
                                                        name_attr.value(),
                                                    ))
                                                }
                                                None => VariableType::Unknown,
                                            },
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
                                None => {
                                    // TODO: Better indication for no return value
                                    child_variable.set_value(VariableValue::Valid(
                                        "<No Return Value>".to_string(),
                                    ));
                                    child_variable.type_name = VariableType::Unknown;
                                }
                            }
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
                    child_variable = self.process_tree(
                        node,
                        child_variable,
                        memory,
                        stack_frame_registers,
                        frame_base,
                        cache,
                    )?;
                }
                // Do not expand this type.
                other => {
                    child_variable.set_value(VariableValue::Error(format!(
                        "<unimplemented: type : {:?}>",
                        other.static_string()
                    )));
                    child_variable.type_name = VariableType::Other("unimplemented".to_string());
                    cache.remove_cache_entry_children(child_variable.variable_key)?;
                }
            }
        }
        cache
            .cache_variable(parent_variable.variable_key, child_variable, memory)
            .map_err(|error| error.into())
    }

    /// Create child variable entries to represent array members and their values.
    #[allow(clippy::too_many_arguments)]
    fn expand_array_member(
        &self,
        unit_ref: UnitOffset,
        cache: &mut VariableCache,
        child_variable: &mut Variable,
        memory: &mut impl MemoryInterface,
        array_member_index: i64,
        stack_frame_registers: &DebugRegisters,
        frame_base: Option<u64>,
    ) -> Result<(), DebugError> {
        let mut array_member_type_tree = self
            .unit
            .header
            .entries_tree(&self.unit.abbreviations, Some(unit_ref))?;
        if let Ok(array_member_type_node) = array_member_type_tree.root() {
            let mut array_member_variable = cache.cache_variable(
                child_variable.variable_key,
                Variable::new(
                    self.unit.header.offset().as_debug_info_offset(),
                    Some(unit_ref),
                ),
                memory,
            )?;
            array_member_variable.member_index = Some(array_member_index);
            // Override the calculated member name with a more 'array-like' name.
            array_member_variable.name = VariableName::Named(format!("__{array_member_index}"));
            array_member_variable.source_location = child_variable.source_location.clone();
            self.process_memory_location(
                array_member_type_node.entry(),
                child_variable,
                &mut array_member_variable,
                memory,
                stack_frame_registers,
                frame_base,
            )?;
            array_member_variable = self.extract_type(
                array_member_type_node,
                child_variable,
                array_member_variable,
                memory,
                stack_frame_registers,
                frame_base,
                cache,
            )?;
            if array_member_index == child_variable.range_lower_bound {
                // Once we know the type of the first member, we can set the array type.
                child_variable.type_name = VariableType::Array {
                    count: child_variable.range_upper_bound as usize,
                    item_type_name: array_member_variable.type_name.clone().to_string(),
                };
                // Once we know the byte_size of the first member, we can set the array byte_size.
                if let Some(array_member_byte_size) = array_member_variable.byte_size {
                    child_variable.byte_size = Some(
                        array_member_byte_size
                            * (child_variable.range_upper_bound - child_variable.range_lower_bound)
                                as u64,
                    );
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
            cache.cache_variable(child_variable.variable_key, array_member_variable, memory)?;
        }
        Ok(())
    }

    /// Process a memory location for a variable, by first evaluating the `byte_size`, and then calling the `self.extract_location`.
    pub(crate) fn process_memory_location(
        &self,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_variable: &Variable,
        child_variable: &mut Variable,
        memory: &mut impl MemoryInterface,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
    ) -> Result<(), DebugError> {
        // The `byte_size` is used for arrays, etc. to offset the memory location of the next element.
        // For nested types, the `byte_size` may need to be calculated as the product of the `byte_size` and array upper bound.
        child_variable.byte_size = child_variable
            .byte_size
            .or_else(|| extract_byte_size(node_die))
            .or_else(|| {
                parent_variable.byte_size.map(|byte_size| {
                    let array_member_count = (parent_variable.range_upper_bound
                        - parent_variable.range_lower_bound)
                        as u64;
                    if array_member_count > 0 {
                        byte_size / array_member_count
                    } else {
                        byte_size
                    }
                })
            });

        if child_variable.memory_location == VariableLocation::Unknown {
            match self.extract_location(
                node_die,
                &parent_variable.memory_location,
                memory,
                stack_frame_registers,
                frame_base,
            ) {
                Ok(location) => match location {
                    // Any expected errors should be handled by one of the variants in the Ok() result.
                    ExpressionResult::Value(value_from_expression) => {
                        if let VariableValue::Valid(_) = &value_from_expression {
                            // The ELF contained the actual value, not just a location to it.
                            child_variable.memory_location = VariableLocation::Value;
                        }
                        child_variable.set_value(value_from_expression);
                    }
                    ExpressionResult::Location(location_from_expression) => {
                        match &location_from_expression {
                            VariableLocation::Unavailable => {
                                child_variable.set_value(VariableValue::Error(
                                    "<value optimized away by compiler, out of scope, or dropped>"
                                        .to_string(),
                                ));
                            }
                            VariableLocation::Error(error_message)
                            | VariableLocation::Unsupported(error_message) => {
                                child_variable
                                    .set_value(VariableValue::Error(error_message.clone()));
                            }
                            VariableLocation::Address(_) | VariableLocation::Value => {
                                child_variable.memory_location = location_from_expression;
                            }

                            VariableLocation::Unknown => {}
                        }
                    }
                },
                Err(debug_error) => {
                    // An Err() result indicates something happened that we have not accounted for. Currently, we support all known location expressions for non-optimized code.
                    child_variable.memory_location = VariableLocation::Error(
                        "Unsupported location expression while resolving the location. Please reduce optimization levels in your build profile.".to_string()
                    );
                    tracing::debug!("Encountered an unsupported location expression while resolving the location for variable {:?}. Please reduce optimization levels in your build profile. : {debug_error:?}", child_variable.name);
                    return Ok(());
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
    /// - Result<_, DebugError>: This happens when we encounter an error we did not expect, and will propogate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - Result<ExpressionResult::Value(),_>:  The value is statically stored in the binary, and can be returned, and has no relevant memory location.
    /// - Result<ExpressionResult::Location(),_>:  One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn extract_location(
        &self,
        node_die: &gimli::DebuggingInformationEntry<GimliReader>,
        parent_location: &VariableLocation,
        memory: &mut impl MemoryInterface,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
    ) -> Result<ExpressionResult, DebugError> {
        let mut attrs = node_die.attrs();
        while let Ok(Some(attr)) = attrs.next() {
            match attr.name() {
                gimli::DW_AT_location
                | gimli::DW_AT_frame_base
                | gimli::DW_AT_data_member_location => match attr.value() {
                    gimli::AttributeValue::Exprloc(expression) => {
                        return match self.evaluate_expression(memory , expression, stack_frame_registers, frame_base) {
                            Ok(result) => Ok(result),
                            Err(error) => {
                                if matches!(error, DebugError::UnwindIncompleteResults { message: _ }) {
                                    Ok(ExpressionResult::Location(VariableLocation::Unavailable))
                                } else {
                                    Err(error)
                                }
                            },
                        };
                    }
                    gimli::AttributeValue::Udata(offset_from_location) => match parent_location {
                        VariableLocation::Address(address) => {
                            let (location, has_overflowed) = address.overflowing_add(
                                offset_from_location,
                            );
                            if has_overflowed {
                                return Err(DebugError::UnwindIncompleteResults {
                                    message: "Overflow calculating variable address".to_string(),
                                });
                            } else {
                                return Ok(ExpressionResult::Location(VariableLocation::Address(location)));
                            }
                        }
                        other_parent_location => {
                            return Ok(ExpressionResult::Location(other_parent_location.clone()));
                        }
                    },
                    gimli::AttributeValue::LocationListsRef(location_list_offset) => {
                        match self.debug_info.locations_section.locations(
                            location_list_offset,
                            self.unit.header.encoding(),
                            self.unit.low_pc,
                            &self.debug_info.address_section,
                            self.unit.addr_base,
                        ) {
                            Ok(mut locations) => {
                                if let Some(program_counter) = stack_frame_registers
                                    .get_program_counter()
                                    .and_then(|reg| reg.value)
                                {
                                    let mut expression: Option<gimli::Expression<GimliReader>> =
                                        None;
                                    while let Some(location) = match locations.next() {
                                        Ok(location_lists_entry) => location_lists_entry,
                                        Err(error) => {
                                            return Ok(ExpressionResult::Location(VariableLocation::Error(format!("Error: Iterating LocationLists for this variable: {:?}", &error))));
                                        }
                                    } {
                                        if program_counter
                                            >= RegisterValue::from(location.range.begin)
                                            && program_counter
                                                < RegisterValue::from(location.range.end)
                                        {
                                            expression = Some(location.data);
                                            break;
                                        }
                                    }
                                    if let Some(valid_expression) = expression {
                                        return match self.evaluate_expression(
                                            memory,
                                            valid_expression,
                                            stack_frame_registers,
                                            frame_base,
                                        ) {
                                            Ok(result) => Ok(result),
                                            Err(error) => {
                                                if matches!(error, DebugError::UnwindIncompleteResults { message: _ }) {
                                                    Ok(ExpressionResult::Location(VariableLocation::Unavailable))
                                                } else {
                                                    Err(error)
                                                }
                                            },
                                        };
                                    } else {
                                        return Ok(ExpressionResult::Location(
                                            VariableLocation::Unavailable,
                                        ));
                                    }
                                } else {
                                    return Ok(ExpressionResult::Location(VariableLocation::Error("Cannot determine variable location without a valid program counter.".to_string())));
                                }
                            }
                            Err(error) => {
                                return Ok(ExpressionResult::Location(VariableLocation::Error(
                                    format!("Error: Resolving variable Location: {:?}", &error),
                                )))
                            }
                        }
                    }
                    other_attribute_value => {
                        return Ok(ExpressionResult::Location(VariableLocation::Unsupported(
                            format!( "Unimplemented: extract_location() Could not extract location from: {:.100}", format!("{other_attribute_value:?}")))))
                    }
                },
                gimli::DW_AT_address_class => {
                    match attr.value() {
                        gimli::AttributeValue::AddressClass(address_class) => {
                            if address_class != gimli::DwAddr(0) {
                                return Ok(ExpressionResult::Location(VariableLocation::Unsupported(format!(
                                    "Unimplemented: extract_location() found unsupported DW_AT_address_class(gimli::DwAddr({address_class:?}))"
                                ))))
                            } else {
                                // We pass on the location of the parent, which will later to be used along with DW_AT_data_member_location to calculate the location of this variable. 
                                return Ok(ExpressionResult::Location(parent_location.clone()));
                            }
                        }
                        other_attribute_value => {
                            return Ok(ExpressionResult::Location(VariableLocation::Unsupported(format!(
                                "Unimplemented: extract_location() found invalid DW_AT_address_class: {:.100}",
                                format!("{other_attribute_value:?}")
                            ))))
                        }
                    }
                }
                _other_attributes => {
                    // These will be handled elsewhere.
                }
            }
        }
        // If we get here, we did not find a location attribute, then leave the value as Unknown.
        Ok(ExpressionResult::Location(VariableLocation::Unknown))
    }

    /// Evaluate a gimli::Expression as a valid memory location.
    /// Return values are implemented as follows:
    /// - Result<_, DebugError>: This happens when we encounter an error we did not expect, and will propogate upwards until the debugger request is failed. NOT GRACEFUL, and should be avoided.
    /// - Result<ExpressionResult::Value(),_>:  The value is statically stored in the binary, and can be returned, and has no relevant memory location.
    /// - Result<ExpressionResult::Location(),_>:  One of the variants of VariableLocation, and needs to be interpreted for handling the 'expected' errors we encounter during evaluation.
    pub(crate) fn evaluate_expression(
        &self,
        memory: &mut impl MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
    ) -> Result<ExpressionResult, DebugError> {
        let pieces =
            self.expression_to_piece(memory, expression, stack_frame_registers, frame_base)?;
        if pieces.is_empty() {
            Ok(ExpressionResult::Location(VariableLocation::Error(
                format!("Error: expr_to_piece() returned 0 results: {pieces:?}"),
            )))
        } else if pieces.len() > 1 {
            Ok(ExpressionResult::Location(VariableLocation::Error(
                "<unsupported memory implementation>".to_string(),
            )))
        } else {
            match &pieces[0].location {
                Location::Empty => {
                    // This means the value was optimized away.
                    Ok(ExpressionResult::Location(VariableLocation::Unavailable))
                }
                Location::Address { address } => {
                    if address.is_zero() {
                        Ok(ExpressionResult::Location(VariableLocation::Error("The value of this variable may have been optimized out of the debug info, by the compiler.".to_string())))
                    } else if !memory.supports_native_64bit_access() {
                        if *address < u32::MAX as u64 {
                            Ok(ExpressionResult::Location(VariableLocation::Address(
                                *address,
                            )))
                        } else {
                            Ok(ExpressionResult::Location(VariableLocation::Error(format!("The memory location for this variable value ({address:#010X}) is invalid. Please report this as a bug."))))
                        }
                    } else {
                        Ok(ExpressionResult::Location(VariableLocation::Address(
                            *address,
                        )))
                    }
                }
                Location::Value { value } => match value {
                    gimli::Value::Generic(value) => Ok(ExpressionResult::Value(
                        VariableValue::Valid(value.to_string()),
                    )),
                    gimli::Value::I8(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::U8(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::I16(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::U16(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::I32(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::U32(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::I64(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::U64(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::F32(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                    gimli::Value::F64(value) => Ok(ExpressionResult::Value(VariableValue::Valid(
                        value.to_string(),
                    ))),
                },
                Location::Register { register } => {
                    if let Some(address) = stack_frame_registers
                        .get_register_by_dwarf_id(register.0)
                        .and_then(|register| register.value)
                    {
                        match address.try_into() {
                            Ok(location) => {
                                if !memory.supports_native_64bit_access() {
                                    if location < u32::MAX as u64 {
                                        Ok(ExpressionResult::Location(VariableLocation::Address(
                                            location,
                                        )))
                                    } else {
                                Ok(ExpressionResult::Location(VariableLocation::Error(format!("The memory location for this variable value ({location:#010X}) is invalid. Please report this as a bug."))))
                                    }
                                } else {
                                    Ok(ExpressionResult::Location(VariableLocation::Address(
                                        location,
                                    )))
                                }
                            },
                            Err(error) => Ok(ExpressionResult::Location(
                                VariableLocation::Error(format!(
                                    "Error: Cannot convert register value to location address: {error:?}"
                                )),
                            )),
                        }
                    } else {
                        Ok(ExpressionResult::Location(VariableLocation::Error(
                            format!("Error: Cannot resolve register: {register:?}"),
                        )))
                    }
                }
                l => Ok(ExpressionResult::Location(VariableLocation::Error(
                    format!(
                        "Unimplemented: extract_location() found a location type: {:.100}",
                        format!("{l:?}")
                    ),
                ))),
            }
        }
    }

    /// Tries to get the result of a DWARF expression in the form of a Piece.
    pub(crate) fn expression_to_piece(
        &self,
        memory: &mut impl MemoryInterface,
        expression: gimli::Expression<GimliReader>,
        stack_frame_registers: &registers::DebugRegisters,
        frame_base: Option<u64>,
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
                    match provide_frame_base(frame_base, &mut evaluation) {
                        Ok(value) => value,
                        Err(value) => return Err(value),
                    }
                }
                EvaluationResult::RequiresRegister {
                    register,
                    base_type,
                } => provide_register(stack_frame_registers, register, base_type, &mut evaluation)?,
                EvaluationResult::RequiresRelocatedAddress(address_index) => {
                    // The address_index as an offset from 0, so just pass it into the next step.
                    evaluation.resume_with_relocated_address(address_index)?
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
        memory: &mut impl MemoryInterface,
    ) {
        if let Some(child_member_index) = child_variable.member_index {
            // If this variable is a member of an array type, and needs special handling to calculate the `memory_location`.
            if let VariableLocation::Address(address) = parent_variable.memory_location {
                if let Some(byte_size) = child_variable.byte_size {
                    let (location, has_overflowed) =
                        address.overflowing_add(child_member_index as u64 * byte_size);

                    if has_overflowed {
                        child_variable.set_value(VariableValue::Error(
                            "Overflow calculating variable address".to_string(),
                        ));
                    } else {
                        child_variable.memory_location = VariableLocation::Address(location);
                    }
                } else {
                    // If this array member doesn't have a byte_size, it may be because it is the first member of an array itself.
                    // In this case, the byte_size will be calculated when the nested array members are resolved.
                    // The first member of an array will have a memory location of the same as it's parent.
                    child_variable.memory_location = parent_variable.memory_location.clone();
                }
            } else {
                child_variable.memory_location = VariableLocation::Unavailable;
            }
        } else if child_variable.memory_location == VariableLocation::Unknown {
            // Non-array members can inherit their memory location from their parent, but only if the parent has a valid memory location.

            // Overriding clippy, to defer the processing of `self.has_address_pointer()` until after the `||` conditions.
            if self.is_pointer(child_variable, parent_variable, unit_ref) {
                match &parent_variable.memory_location {
                    VariableLocation::Address(address) => {
                        // Now, retrieve the location by reading the adddress pointed to by the parent variable.
                        child_variable.memory_location = match memory.read_word_32(*address) {
                            Ok(memory_location) => {
                                VariableLocation::Address(memory_location as u64)
                            }
                            Err(error) => {
                                tracing::debug!("Failed to read referenced variable address from memory location {} : {error}.", parent_variable.memory_location);
                                VariableLocation::Error(format!("Failed to read referenced variable address from memory location {} : {error}.", parent_variable.memory_location))
                            }
                        };
                    }
                    other => {
                        child_variable.memory_location = VariableLocation::Unsupported(format!(
                            "Location {other:?} not supported for referenced variables."
                        ));
                    }
                }
            } else {
                // If the parent variable is not a pointer, or it is a pointer to the actual data location
                // (not the address of the data location) then it can inherit it's memory location from it's parent.
                child_variable.memory_location = parent_variable.memory_location.clone();
            }
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

/// Gets necessary register informations for the DWARF resolver.
fn provide_register(
    stack_frame_registers: &DebugRegisters,
    register: gimli::Register,
    base_type: UnitOffset,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    let raw_value = match stack_frame_registers
        .get_register_by_dwarf_id(register.0)
        .and_then(|reg| reg.value)
    {
        Some(raw_value) => {
            if base_type != gimli::UnitOffset(0) {
                return Err(DebugError::UnwindIncompleteResults {
                    message: format!("Unimplemented: Support for type {base_type:?} in `RequiresRegister` request is not yet implemented."
                )});
            }
            raw_value
        }
        None => {
            return Err(DebugError::UnwindIncompleteResults {
                    message: format!("Error while calculating `Variable::memory_location`. No value for register #:{}.",
                    register.0
                )});
        }
    };
    Ok(evaluation.resume_with_register(gimli::Value::Generic(raw_value.try_into()?))?)
}

/// Gets necessary framebase informations for the DWARF resolver.
fn provide_frame_base(
    frame_base: Option<u64>,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    let frame_base = if let Some(frame_base) = frame_base {
        frame_base
    } else {
        return Err(DebugError::UnwindIncompleteResults {
            message: "Cannot unwind `Variable` location without a valid frame base address.)"
                .to_string(),
        });
    };
    Ok(match evaluation.resume_with_frame_base(frame_base) {
        Ok(evaluation_result) => evaluation_result,
        Err(error) => {
            return Err(DebugError::UnwindIncompleteResults {
                message: format!("Error while calculating `Variable::memory_location`:{error}."),
            })
        }
    })
}

/// Reads memory requested by the DWARF resolver.
fn read_memory(
    size: u8,
    memory: &mut impl MemoryInterface,
    address: u64,
    evaluation: &mut gimli::Evaluation<EndianReader>,
) -> Result<EvaluationResult<EndianReader>, DebugError> {
    fn decode_error(error: Vec<u8>) -> DebugError {
        DebugError::UnwindIncompleteResults {
            message: format!("Unexpected error while dereferencing debug expressions from target memory: {error:?}. Please report this as a bug.")
        }
    }

    fn read_error(error: Error) -> DebugError {
        DebugError::UnwindIncompleteResults {
            message: format!("Unexpected error while reading debug expressions from target memory: {error:?}. Please report this as a bug.")
        }
    }

    fn size_error(x: u8) -> DebugError {
        DebugError::UnwindIncompleteResults {
            message: format!(
                "Unimplemented: Requested memory with size {x}, which is not supported yet."
            ),
        }
    }

    let mut buff = vec![0u8; size as usize];
    memory.read(address, &mut buff).map_err(read_error)?;
    Ok(match size {
        1 => evaluation.resume_with_memory(gimli::Value::U8(buff[0]))?,
        2 => evaluation.resume_with_memory(gimli::Value::U16(u16::from_le_bytes(
            buff.try_into().map_err(decode_error)?,
        )))?,
        4 => evaluation.resume_with_memory(gimli::Value::U32(u32::from_le_bytes(
            buff.try_into().map_err(decode_error)?,
        )))?,
        x => {
            return Err(size_error(x));
        }
    })
}
