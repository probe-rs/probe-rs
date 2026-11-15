//! Builds the names of types, and reads the ranges of arrays.

use std::{collections::HashSet, iter::successors, ops::Range};

use crate::{DebugError, debug_info::*, variable::*};
use gimli::{DebugInfoOffset, DebuggingInformationEntry, UnitOffset};

use super::{UnitInfo, attribute_string, extract_name};

impl UnitInfo {
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
    pub(super) fn extract_array_range(
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

    pub(super) fn extract_named_type(
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

    pub(super) fn pointer_from_name(&self, name: Option<String>) -> VariableType {
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

    /// Names of enclosing namespace, module, function, and type DIEs, crate first.
    pub(crate) fn enclosing_path(
        &self,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Vec<String> {
        self.parent_names(debug_info, entry, is_enclosing_name_tag)
    }

    /// Names of enclosing `DW_TAG_namespace` / `DW_TAG_module` DIEs, crate first.
    pub(crate) fn namespace_path(
        &self,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Vec<String> {
        self.parent_names(debug_info, entry, |tag| {
            matches!(tag, gimli::DW_TAG_namespace | gimli::DW_TAG_module)
        })
    }

    /// Names of the enclosing DIEs whose tag `include` accepts, crate first.
    fn parent_names(
        &self,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
        include: fn(gimli::DwTag) -> bool,
    ) -> Vec<String> {
        let parents = successors(self.parent_offset(entry.offset()), |&offset| {
            self.parent_offset(offset)
        });
        let mut segments = parents
            .filter_map(|offset| self.unit.entry(offset).ok())
            .filter(|die| include(die.tag()))
            .filter_map(|die| extract_name(debug_info, &self.unit, &die).ok().flatten())
            .collect::<Vec<_>>();
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
