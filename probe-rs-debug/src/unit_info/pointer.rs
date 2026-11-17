//! Resolves the locations that pointers and array members refer to.

use crate::{DebugError, debug_info::*, extract_alignment, extract_byte_size, variable::*};
use gimli::UnitOffset;
use probe_rs::MemoryInterface;

use super::UnitInfo;

impl UnitInfo {
    /// A helper function, to handle memory_location for special cases, such as array members, pointers, and intermediate nodes.
    /// Normally, the memory_location is calculated before the type is calculated,
    ///     but special cases require the type related info of the variable to correctly compute the memory_location.
    pub(super) fn handle_memory_location_special_cases(
        &self,
        unit_ref: UnitOffset,
        child_variable: &mut Variable,
        parent_variable: &Variable,
        memory: &mut dyn MemoryInterface,
    ) {
        let location = if let VariableName::Indexed(child_member_index) = child_variable.name {
            array_member_location(child_member_index, child_variable, parent_variable)
        } else if self.is_pointer(child_variable, parent_variable, unit_ref) {
            self.referenced_location(unit_ref, child_variable, parent_variable, memory)
        } else if child_variable.memory_location == VariableLocation::Unknown {
            // A variable that is not a referenced value shares the location of its parent, for
            // example an intermediate node of a struct.
            parent_variable.memory_location.clone()
        } else {
            return;
        };

        child_variable.memory_location = location;
    }

    /// The location of the value that the pointer `parent_variable` refers to.
    fn referenced_location(
        &self,
        unit_ref: UnitOffset,
        child_variable: &Variable,
        parent_variable: &Variable,
        memory: &mut dyn MemoryInterface,
    ) -> VariableLocation {
        if !matches!(
            parent_variable.memory_location,
            VariableLocation::Address(_)
                | VariableLocation::RegisterValue(_)
                | VariableLocation::Composite(_)
                | VariableLocation::Value
        ) {
            return VariableLocation::Unsupported(format!(
                "Location {:?} not supported for referenced variables.",
                parent_variable.memory_location
            ));
        }

        let address = match self.pointer_address(parent_variable, memory) {
            Ok(address) => address,
            Err(error) => {
                tracing::debug!(
                    "Failed to read referenced variable address from memory location {} : {error}.",
                    parent_variable.memory_location
                );
                return VariableLocation::Error(format!(
                    "Failed to read referenced variable address from memory location {} : {error}.",
                    parent_variable.memory_location
                ));
            }
        };

        let alignment = self
            .unit
            .entry(unit_ref)
            .ok()
            .and_then(|entry| extract_alignment(&entry));

        match object_at(address, alignment, child_variable.byte_size) {
            Some(address) => VariableLocation::Address(address),
            None if address == 0 => VariableLocation::Error("<null pointer>".to_string()),
            None => VariableLocation::Error(format!("<dangling pointer: {address:#010X}>")),
        }
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
        let is_variant_pointer = has_pointer_name(child_variable)
            && matches!(parent_variable.role, VariantRole::VariantPart(_));
        let has_pointer_type_name = parent_variable
            .type_name
            .ident()
            .is_some_and(|name| name.starts_with('*'));
        if is_variant_pointer || has_pointer_type_name {
            return true;
        }
        if !matches!(&parent_variable.type_name, VariableType::Pointer(_)) {
            return false;
        }

        let points_at_a_value = matches!(child_variable.type_name, VariableType::Base(_))
            || matches!(child_variable.type_name, VariableType::Struct(ref type_name) if type_name.ident_stem().starts_with("&str"))
            || has_pointer_name(child_variable);
        points_at_a_value
            || self.has_address_pointer(unit_ref).unwrap_or_else(|error| {
                child_variable.set_value(VariableValue::Error(format!(
                    "Failed to determine if a struct has variant or generic type fields: {error}"
                )));
                false
            })
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
}

/// The location of the array member at `index`.
fn array_member_location(
    index: u64,
    child_variable: &Variable,
    parent_variable: &Variable,
) -> VariableLocation {
    if !matches!(
        parent_variable.memory_location,
        VariableLocation::Address(_)
            | VariableLocation::RegisterValue(_)
            | VariableLocation::Composite(_)
    ) {
        return VariableLocation::Unavailable;
    }

    // Push the array member to the proper location according to its index.
    match child_variable.byte_size {
        Some(byte_size) => parent_variable
            .memory_location
            .offset_by(index * byte_size, Some(byte_size)),
        // If this array member doesn't have a byte_size, it may be because it is the first member of an array itself.
        // In this case, the byte_size will be calculated when the nested array members are resolved.
        // The first member of an array will have a memory location of the same as it's parent.
        None => parent_variable.memory_location.clone(),
    }
}

fn has_pointer_name(variable: &Variable) -> bool {
    matches!(variable.name, VariableName::Named(ref name) if name.starts_with('*'))
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

#[cfg(test)]
mod test {
    use super::object_at;

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
}
