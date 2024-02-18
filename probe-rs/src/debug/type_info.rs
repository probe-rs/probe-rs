use std::collections::HashMap;

use gimli::{DebugInfoOffset, DebuggingInformationEntry, UnitOffset};

use super::{
    extract_byte_size,
    unit_info::{extract_name, UnitInfo},
    DebugInfo, GimliReader,
};

#[derive(Debug)]
pub struct TypeCache {
    visited_types: HashMap<DebugInfoOffset, Option<TypeInfo>>,
}

impl TypeCache {
    pub fn type_info_from_unit_offset(
        &mut self,
        debug_info: &DebugInfo,
        unit_info: &UnitInfo,
        reference: UnitOffset,
    ) -> Result<TypeInfo, TypeInfoError> {
        self.type_info_from_info_offset(
            debug_info,
            unit_info,
            reference
                .to_debug_info_offset(&unit_info.unit.header)
                .unwrap(),
        )
    }

    pub fn type_info_from_info_offset(
        &mut self,
        debug_info: &DebugInfo,
        unit_info: &UnitInfo,
        reference: DebugInfoOffset,
    ) -> Result<TypeInfo, TypeInfoError> {
        let unit_reference = reference
            .to_unit_offset(&unit_info.unit.header)
            .ok_or_else(|| TypeInfoError::DebugInfoOffsetOutOfRange(reference))?;

        let entry = unit_info
            .unit
            .entry(unit_reference)
            .expect("Unit info mismatch");

        self.type_info_from_entry(debug_info, unit_info, &entry)
    }

    pub fn type_info_from_entry(
        &mut self,
        debug_info: &DebugInfo,
        unit_info: &UnitInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Result<TypeInfo, TypeInfoError> {
        let type_name = extract_name(debug_info, entry)
            .unwrap_or_else(|err| Some(format!("Error: Failed to decode type name: {err:?}")));

        let info_reference = entry
            .offset()
            .to_debug_info_offset(&unit_info.unit.header)
            .unwrap();

        if let Some(type_info) = self.visited_types.get(&info_reference) {
            if let Some(type_info) = type_info {
                tracing::trace!("Type info already visited: {:?}", type_info);
                return Ok(type_info.clone());
            } else {
                tracing::warn!("Recursive type detected: type at debug info offset {:#010x} already encounterd but not resolved", info_reference.0);
                let mut type_info = TypeInfo::new(info_reference, type_name);
                type_info.kind = TypeKind::Error("Recursive type detected".to_string());
                return Ok(type_info);
            }
        }

        self.visited_types.insert(info_reference, None);

        let mut type_info = TypeInfo::new(info_reference, type_name);

        let byte_size = extract_byte_size(&entry);

        match entry.tag() {
            gimli::DW_TAG_base_type => {
                type_info.kind = TypeKind::Base { byte_size };
            }
            gimli::DW_TAG_pointer_type => match entry.attr(gimli::DW_AT_type) {
                Ok(Some(attr)) => {
                    let pointed_to = self.process_type_attribute(unit_info, &attr, debug_info)?;

                    let byte_size =
                        extract_byte_size(&entry).unwrap_or(unit_info.address_size() as u64);

                    type_info.kind = TypeKind::Pointer {
                        byte_size,
                        pointed_to: Box::new(pointed_to),
                    };
                }
                Ok(None) => {
                    tracing::debug!("Pointer type without type attribute");
                    type_info.kind = TypeKind::Unknown;
                }
                Err(error) => {
                    type_info.kind = TypeKind::Error(format!(
                        "Error: Failed to decode pointer reference: {error:?}"
                    ));
                }
            },
            gimli::DW_TAG_structure_type => {
                self.extract_struct_type_info(debug_info, unit_info, entry, &mut type_info)?;
            }
            gimli::DW_TAG_subroutine_type => {
                // TODO: Actually discover things about the subroutine
                type_info.kind = TypeKind::Subroutine;
            }
            gimli::DW_TAG_enumeration_type => {
                // TODO: Properly handle this
                type_info.kind = TypeKind::Enum;
            }
            gimli::DW_TAG_array_type => {
                let mut tree = unit_info
                    .unit
                    .header
                    .entries_tree(&unit_info.unit.abbreviations, Some(entry.offset()))?;

                let mut child_nodes = tree.root()?.children();

                let mut array_len = None;

                // We assume there is a single child node which has the range information
                while let Some(child) = child_nodes.next()? {
                    if let Some(count) = child.entry().attr_value(gimli::DW_AT_count).ok().flatten()
                    {
                        array_len = count.udata_value();
                    }

                    if let Some(upper_bound) = child
                        .entry()
                        .attr_value(gimli::DW_AT_upper_bound)
                        .ok()
                        .flatten()
                    {
                        array_len = upper_bound.udata_value();
                    }

                    if array_len.is_some() {
                        break;
                    }
                }

                match entry.attr(gimli::DW_AT_type) {
                    Ok(Some(attr)) => {
                        let member_type =
                            self.process_type_attribute(unit_info, &attr, debug_info)?;

                        // TODO: This is Rust specific
                        type_info.set_name(Some(format!(
                            "[{}; {}]",
                            member_type.name().unwrap_or("<unknown>"),
                            array_len
                                .map(|l| l.to_string())
                                .as_deref()
                                .unwrap_or("<unknown>")
                        )));

                        type_info.kind = TypeKind::Array {
                            ty: Box::new(member_type),
                            len: array_len,
                        };
                    }
                    Ok(None) => {
                        tracing::debug!("Array type without type attribute");
                        type_info.kind = TypeKind::Unknown;
                    }
                    Err(error) => {
                        type_info.kind = TypeKind::Error(format!(
                            "Error: Failed to decode array type: {error:?}"
                        ));
                    }
                }
            }
            gimli::DW_TAG_union_type => {
                self.extract_union_type_info(unit_info, debug_info, entry, &mut type_info)?;
            }
            other @ (gimli::DW_TAG_typedef
            | gimli::DW_TAG_const_type
            | gimli::DW_TAG_volatile_type) => match entry.attr(gimli::DW_AT_type) {
                Ok(Some(attr)) => {
                    let modified_type_info =
                        self.process_type_attribute(unit_info, &attr, debug_info)?;

                    let modifier = match other {
                        gimli::DW_TAG_typedef => Modifier::Typedef,
                        gimli::DW_TAG_const_type => Modifier::Const,
                        gimli::DW_TAG_volatile_type => Modifier::Volatile,
                        _ => unreachable!(),
                    };

                    type_info.kind = TypeKind::Modified {
                        modifier,
                        ty: Box::new(modified_type_info),
                    };
                }

                Ok(None) => {
                    type_info.kind = unit_info.language.process_tag_with_no_type_type_info(other);
                }
                Err(_error) => todo!("Handle typedef without type"),
            },
            other => todo!("Handle type: {}", other),
        }

        self.visited_types
            .insert(info_reference, Some(type_info.clone()));

        Ok(type_info)
    }

    fn extract_struct_type_info(
        &mut self,
        debug_info: &DebugInfo,
        unit_info: &UnitInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        type_info: &mut TypeInfo,
    ) -> Result<(), TypeInfoError> {
        let byte_size = extract_byte_size(node);

        let mut members = Vec::new();

        let mut tree = unit_info
            .unit
            .header
            .entries_tree(&unit_info.unit.abbreviations, Some(node.offset()))?;

        let mut child_nodes = tree.root()?.children();

        while let Some(child_node) = child_nodes.next()? {
            match child_node.entry().tag() {
                gimli::DW_TAG_variant_part => {
                    type_info.kind = TypeKind::VariantStruct { byte_size };

                    if let Some(discr) = child_node
                        .entry()
                        .attr_value(gimli::DW_AT_discr)
                        .ok()
                        .flatten()
                    {
                        tracing::debug!("Variant part with discriminator: {:?}", discr);
                    }

                    return Ok(());
                }
                gimli::DW_TAG_member => {
                    let name = extract_name(debug_info, child_node.entry()).ok().flatten();
                    // TODO: Offset
                    let offset = child_node
                        .entry()
                        .attr_value(gimli::DW_AT_data_member_location)
                        .ok()
                        .flatten()
                        .and_then(|val| val.udata_value());

                    let member_type = if let Some(member_type) =
                        child_node.entry().attr(gimli::DW_AT_type).ok().flatten()
                    {
                        self.process_type_attribute(unit_info, &member_type, debug_info)?
                    } else {
                        TypeInfo::new(DebugInfoOffset(0), None)
                    };

                    members.push(StructMember {
                        name,
                        ty: Box::new(member_type),
                        offset,
                    });
                }

                other => {
                    tracing::trace!("struct: Skipping child node with tag: {}", other);
                }
            }
        }
        type_info.kind = TypeKind::Struct { byte_size, members };

        Ok(())
    }

    fn extract_union_type_info(
        &mut self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        type_info: &mut TypeInfo,
    ) -> Result<(), TypeInfoError> {
        tracing::debug!("Extracting union type info for {:?}", type_info);

        let byte_size = extract_byte_size(node);
        let mut members = Vec::new();

        let mut tree = unit_info
            .unit
            .header
            .entries_tree(&unit_info.unit.abbreviations, Some(node.offset()))?;

        let mut child_nodes = tree.root()?.children();

        while let Some(child_node) = child_nodes.next()? {
            match child_node.entry().tag() {
                gimli::DW_TAG_member => {
                    let member_type = if let Some(member_type) =
                        child_node.entry().attr(gimli::DW_AT_type).ok().flatten()
                    {
                        self.process_type_attribute(unit_info, &member_type, debug_info)?
                    } else {
                        TypeInfo::new(DebugInfoOffset(0), None)
                    };

                    members.push(Box::new(member_type));
                }
                other => {
                    tracing::trace!("union: Skipping child node with tag: {}", other);
                }
            }
        }

        type_info.kind = TypeKind::Union { byte_size, members };

        Ok(())
    }

    fn process_type_attribute(
        &mut self,
        unit_info: &UnitInfo,
        attr: &gimli::Attribute<GimliReader>,
        debug_info: &DebugInfo,
    ) -> Result<TypeInfo, TypeInfoError> {
        let type_info;
        match attr.value() {
            gimli::AttributeValue::UnitRef(unit_ref) => {
                // Now resolve the referenced tree node for the type.
                let mut type_tree = unit_info
                    .unit
                    .header
                    .entries_tree(&unit_info.unit.abbreviations, Some(unit_ref))?;
                let referenced_type_tree_node = type_tree.root()?;

                type_info = self.type_info_from_entry(
                    debug_info,
                    unit_info,
                    referenced_type_tree_node.entry(),
                )?;
            }

            _other_attribute_value => {
                // TODO(typeinfo): This is bad, if the type attribute is not a unit ref, we should record this in the calling function
                let offset = DebugInfoOffset(0);
                type_info = TypeInfo::new(offset, None);
            }
        }

        Ok(type_info)
    }

    pub fn new() -> Self {
        Self {
            visited_types: HashMap::new(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum TypeInfoError {
    #[error("Error reading DWARF debug information")]
    Gimli(#[from] gimli::Error),
    #[error("DIE offset {:#010x} is out of range of the compile unit", (.0).0)]
    DebugInfoOffsetOutOfRange(DebugInfoOffset),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeInfo {
    offset: DebugInfoOffset,
    name: Option<String>,

    pub kind: TypeKind,
}

impl TypeInfo {
    pub fn new(offset: DebugInfoOffset, name: Option<String>) -> Self {
        Self {
            offset,
            name,
            kind: TypeKind::Unknown,
        }
    }

    pub fn offset(&self) -> DebugInfoOffset {
        self.offset
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn set_name(&mut self, name: Option<String>) {
        self.name = name;
    }

    pub fn byte_size(&self) -> Option<u64> {
        match &self.kind {
            TypeKind::Base { byte_size } => *byte_size,
            TypeKind::Struct { byte_size, .. } => *byte_size,
            TypeKind::Pointer { byte_size, .. } => Some(*byte_size),
            TypeKind::Union { byte_size, .. } => *byte_size,
            TypeKind::Modified { ty, .. } => ty.byte_size(),
            TypeKind::Array { ty, len } => ty
                .byte_size()
                .and_then(|entry_size| len.map(|len| len * entry_size)),
            TypeKind::Enum => todo!(),
            TypeKind::VariantStruct { byte_size } => *byte_size,
            TypeKind::Subroutine | TypeKind::Unknown | TypeKind::Error(_) => None,
        }
    }

    pub fn resolved_type(&self) -> TypeInfo {
        match &self.kind {
            TypeKind::Modified { ty, modifier } => {
                match modifier {
                    Modifier::Typedef => {
                        // A typedef keeps it original definition location and name,
                        // but the type kind is the one of the modified type.
                        let mut resolved = *ty.clone();
                        resolved.kind = ty.resolved_type().kind;
                        resolved
                    }
                    Modifier::Const => {
                        // A const modifier means that everything is taken from the modified type,
                        // there is just a const flag added. This is currently just done in the name
                        let mut resolved = ty.resolved_type();
                        resolved.name = resolved.name.as_ref().map(|n| format!("const {}", n));
                        resolved
                    }
                    Modifier::Volatile => {
                        // A volatile modifier means that everything is taken from the modified type,
                        // there is just a volatile flag added. This is currently just done in the name
                        let mut resolved = ty.resolved_type();
                        resolved.name = resolved.name.as_ref().map(|n| format!("volatile {}", n));
                        resolved
                    }
                }
            }
            _ => self.clone(),
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum TypeKind {
    Base {
        /// Size of the type in bytes
        ///
        /// This should always be present for base types, but might be missing due
        /// to incorrect debug information.
        byte_size: Option<u64>,
    },
    Struct {
        /// Size of the type in bytes
        ///
        /// This should always be present for structs, but might be missing due
        /// to incorrect debug information.
        byte_size: Option<u64>,

        members: Vec<StructMember>,
    },
    Modified {
        /// The type that is modified
        ty: Box<TypeInfo>,
        /// The modifier
        modifier: Modifier,
    },
    Pointer {
        byte_size: u64,
        /// The type that is pointed to
        pointed_to: Box<TypeInfo>,
    },
    Union {
        byte_size: Option<u64>,
        members: Vec<Box<TypeInfo>>,
    },
    /// A tempory name for struct which as a variant part
    VariantStruct {
        byte_size: Option<u64>,
    },
    Subroutine,
    Unknown,
    /// Error processing the type
    Error(String),
    Array {
        ty: Box<TypeInfo>,
        len: Option<u64>,
    },
    /// Enumeration
    Enum,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum Modifier {
    Volatile,
    Const,
    Typedef,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct StructMember {
    pub name: Option<String>,
    pub ty: Box<TypeInfo>,
    pub offset: Option<u64>,
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use gimli::DebugInfoOffset;

    use crate::debug::{
        type_info::{TypeCache, TypeKind},
        DebugInfo,
    };

    fn get_rust_debug_info() -> DebugInfo {
        let manifest_dir = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"));

        let debug_info = manifest_dir.join("tests/debug-unwind-tests/nRF52833_xxAA.elf");

        DebugInfo::from_file(debug_info).unwrap()
    }

    fn get_c_debug_info() -> DebugInfo {
        let manifest_dir = PathBuf::from(std::env!("CARGO_MANIFEST_DIR"));

        let debug_info = manifest_dir.join("tests/debug-unwind-tests/nRF5340_lang_c.elf");

        DebugInfo::from_file(debug_info).unwrap()
    }

    #[test]
    fn base_type() {
        //  0x00000143:   DW_TAG_base_type
        //                DW_AT_name  ("u8")
        //                DW_AT_encoding  (DW_ATE_unsigned)
        //                DW_AT_byte_size (0x01)

        let mut type_cache = TypeCache::new();

        let debug_info = get_rust_debug_info();
        // TODO: This is ugly

        let type_info = type_cache
            .type_info_from_info_offset(
                &debug_info,
                &debug_info.unit_infos[0],
                DebugInfoOffset(0x143),
            )
            .unwrap();

        assert_eq!(type_info.name(), Some("u8"));
        assert_eq!(type_info.byte_size(), Some(1));
        assert_eq!(type_info.kind, TypeKind::Base { byte_size: Some(1) });
    }

    #[test]
    fn pointer_type() {
        // 0x00001a3e:   DW_TAG_base_type
        //               DW_AT_name  ("u8")
        //               DW_AT_encoding  (DW_ATE_unsigned)
        //               DW_AT_byte_size (0x01)

        // 0x00001a8a:   DW_TAG_pointer_type
        //               DW_AT_type  (0x00001a3e "u8")
        //               DW_AT_address_class (0x00000000)

        let mut type_cache = TypeCache::new();

        let debug_info = get_rust_debug_info();
        // TODO: This is ugly

        let type_info = type_cache
            .type_info_from_info_offset(
                &debug_info,
                &debug_info.unit_infos[5],
                DebugInfoOffset(0x1a8a),
            )
            .unwrap();

        let pointed_to = Box::new(
            type_cache.visited_types[&DebugInfoOffset(0x1a3e)]
                .clone()
                .unwrap(),
        );

        assert_eq!(type_info.name(), None);
        assert_eq!(type_info.byte_size(), Some(4));
        assert_eq!(
            type_info.kind,
            TypeKind::Pointer {
                byte_size: 4,
                pointed_to
            }
        );
    }

    #[test]
    fn struct_type() {
        // 0x000000bc:   DW_TAG_pointer_type
        //                 DW_AT_byte_size (0x04)
        //                 DW_AT_type  (0x000000b1 "void (const void *)")
        //
        // 0x00000082:   DW_TAG_structure_type
        //                 DW_AT_name  ("_isr_table_entry")
        //                 DW_AT_byte_size (0x08)
        //                 DW_AT_decl_file ("/home/dominik/ncs/v2.5.2/zephyr/include/zephyr/sw_isr_table.h")
        //                 DW_AT_decl_line (36)
        //                 DW_AT_decl_column   (0x08)
        //                 DW_AT_sibling   (0x000000aa)
        //
        // 0x0000008f:     DW_TAG_member
        //                   DW_AT_name    ("arg")
        //                   DW_AT_decl_file   ("/home/dominik/ncs/v2.5.2/zephyr/include/zephyr/sw_isr_table.h")
        //                   DW_AT_decl_line   (37)
        //                   DW_AT_decl_column (0x0e)
        //                   DW_AT_type    (0x000000aa "const void *")
        //                   DW_AT_data_member_location    (0x00)
        //
        // 0x0000009c:     DW_TAG_member
        //                   DW_AT_name    ("isr")
        //                   DW_AT_decl_file   ("/home/dominik/ncs/v2.5.2/zephyr/include/zephyr/sw_isr_table.h")
        //                   DW_AT_decl_line   (38)
        //                   DW_AT_decl_column (0x09)
        //                   DW_AT_type    (0x000000bc "void (*)(const void *)")
        //                   DW_AT_data_member_location    (0x04)

        let mut type_cache = TypeCache::new();

        let debug_info = get_c_debug_info();
        // TODO: This is ugly

        let type_info = type_cache
            .type_info_from_info_offset(
                &debug_info,
                &debug_info.unit_infos[0],
                DebugInfoOffset(0x82),
            )
            .unwrap();

        assert_eq!(type_info.name(), Some("_isr_table_entry"));
        assert_eq!(type_info.byte_size(), Some(8));
    }

    #[test]
    fn rust_enum() {
        // This tests parsing of a Rust enum, represent as a struct with a variant part

        //  0x00000434:       DW_TAG_structure_type
        //                      DW_AT_name  ("Option<cortex_m::peripheral::Peripherals>")
        //                      DW_AT_byte_size (0x01)
        //                      DW_AT_alignment (1)
        //  0x0000043b:         DW_TAG_variant_part
        //                        DW_AT_discr   (0x00000440)
        //
        //  0x00000440:           DW_TAG_member
        //                          DW_AT_type  (0x0000040c "u8")
        //                          DW_AT_alignment (1)
        //                          DW_AT_data_member_location  (0x00)
        //                          DW_AT_artificial    (true)
        //
        //  0x00000447:           DW_TAG_variant
        //                          DW_AT_discr_value   (0x00)
        //
        //  0x00000449:             DW_TAG_member
        //                            DW_AT_name    ("None")
        //                            DW_AT_type    (0x00000464 "core::option::Option<cortex_m::peripheral::Peripherals>::None<cortex_m::peripheral::Peripherals>")
        //                            DW_AT_alignment   (1)
        //                            DW_AT_data_member_location    (0x00)
        //
        //  0x00000454:             NULL
        //
        //  0x00000455:           DW_TAG_variant
        //                          DW_AT_discr_value   (0x01)
        //
        //  0x00000457:             DW_TAG_member
        //                            DW_AT_name    ("Some")
        //                            DW_AT_type    (0x00000475 "core::option::Option<cortex_m::peripheral::Peripherals>::Some<cortex_m::peripheral::Peripherals>")
        //                            DW_AT_alignment   (1)
        //                            DW_AT_data_member_location    (0x00)
        //
        //  0x00000462:             NULL
        //
        //  0x00000463:           NULL

        let mut type_cache = TypeCache::new();

        let debug_info = get_rust_debug_info();

        let type_info = type_cache
            .type_info_from_info_offset(
                &debug_info,
                &debug_info.unit_infos[1],
                DebugInfoOffset(0x434),
            )
            .unwrap();

        assert_eq!(
            type_info.name(),
            Some("Option<cortex_m::peripheral::Peripherals>")
        );
        assert_eq!(type_info.byte_size(), Some(1));
    }
}
