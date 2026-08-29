use crate::{
    DebugError, DebugInfo, GimliReader, ObjectRef, Variable, VariableCache, VariableLocation,
    VariableName, VariableNodeType, VariableType, VariableValue,
    function_die::Die,
    language::{
        ProgrammingLanguage,
        value::{Value, format_float},
    },
    stack_frame::StackFrameInfo,
    unit_info::UnitInfo,
};

use gimli::DebuggingInformationEntry;
use probe_rs::MemoryInterface;

struct Slice<'a> {
    length: u64,
    data_ptr: &'a Variable,
}

#[derive(Debug, Clone)]
pub struct Rust;
impl Rust {
    fn try_get_slice<'a>(variable: &'a Variable, cache: &'a VariableCache) -> Option<Slice<'a>> {
        fn is_field(var: &Variable, name: &str) -> bool {
            matches!(var.name, VariableName::Named(ref var_name) if var_name == name)
        }

        Some(Slice {
            // Do we have a length?
            length: cache
                .get_children(variable.variable_key)
                .find(|c| is_field(c, "length"))
                .and_then(|field| match &field.value {
                    VariableValue::Valid(length_value) => Some(length_value),
                    _ => None,
                })
                .and_then(|length_str| length_str.parse().ok())?,

            // Do we have a data pointer?
            data_ptr: cache
                .get_children(variable.variable_key)
                .find(|c| is_field(c, "data_ptr"))?,
        })
    }

    /// Reads a `usize`, whose width is the pointer width of the target.
    fn read_usize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &VariableCache,
    ) -> VariableValue {
        match variable.byte_size {
            Some(1) => u8::get_value(variable, memory, cache).into(),
            Some(2) => u16::get_value(variable, memory, cache).into(),
            Some(8) => u64::get_value(variable, memory, cache).into(),
            Some(16) => u128::get_value(variable, memory, cache).into(),
            _ => u32::get_value(variable, memory, cache).into(),
        }
    }

    /// Reads an `isize`, whose width is the pointer width of the target.
    fn read_isize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &VariableCache,
    ) -> VariableValue {
        match variable.byte_size {
            Some(1) => i8::get_value(variable, memory, cache).into(),
            Some(2) => i16::get_value(variable, memory, cache).into(),
            Some(8) => i64::get_value(variable, memory, cache).into(),
            Some(16) => i128::get_value(variable, memory, cache).into(),
            _ => i32::get_value(variable, memory, cache).into(),
        }
    }

    /// Writes a `usize`, whose width is the pointer width of the target.
    fn update_usize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match variable.byte_size {
            Some(1) => u8::update_value(variable, memory, new_value),
            Some(2) => u16::update_value(variable, memory, new_value),
            Some(8) => u64::update_value(variable, memory, new_value),
            Some(16) => u128::update_value(variable, memory, new_value),
            _ => u32::update_value(variable, memory, new_value),
        }
    }

    /// Writes an `isize`, whose width is the pointer width of the target.
    fn update_isize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match variable.byte_size {
            Some(1) => i8::update_value(variable, memory, new_value),
            Some(2) => i16::update_value(variable, memory, new_value),
            Some(8) => i64::update_value(variable, memory, new_value),
            Some(16) => i128::update_value(variable, memory, new_value),
            _ => i32::update_value(variable, memory, new_value),
        }
    }

    /// Replaces *const data pointer with *const [data; len] in slices.
    ///
    /// This function may return `Ok(())` even if it does not modify the variable.
    fn expand_slice(
        &self,
        debug_info: &DebugInfo,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(slice) = Self::try_get_slice(variable, cache) else {
            return Ok(());
        };

        // Turn the data pointer into an array.
        let pointer_key = slice.data_ptr.variable_key;
        let length = slice.length;

        let Some(mut pointee) = cache.get_children(pointer_key).next().cloned() else {
            return Ok(());
        };

        // Do we know the type of the data?
        let Some(type_node_offset) = pointee.type_node_offset else {
            return Ok(());
        };

        // Let's just remove the pointer. While it may be interesting where the data is, the
        // address can be read using the debugger, and is otherwise just noise on the UI.
        cache.remove_cache_entry(pointer_key)?;

        // Replace the pointee type with an array of known length. We don't have to modify the
        // memory location, as the pointer is already pointing to the first element of the array.
        pointee.parent_key = ObjectRef::Invalid;
        pointee.variable_key = ObjectRef::Invalid;
        pointee.value = VariableValue::Empty;
        pointee.type_name = VariableType::Array {
            item_type_name: { Box::new(pointee.type_name) },
            count: length as usize,
        };
        pointee.variable_node_type = VariableNodeType::RecurseToBaseType;

        cache.add_variable(variable.variable_key, &mut pointee)?;

        let (member_unit, array_member_type_node) =
            debug_info.entry_at_debug_info_offset(type_node_offset)?;

        let member_range = 0..length;
        member_unit.expand_array_members(
            debug_info,
            &array_member_type_node,
            cache,
            &mut pointee,
            memory,
            &[member_range],
            frame_info,
        )?;

        Ok(())
    }
}

impl ProgrammingLanguage for Rust {
    fn read_variable_value(
        &self,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> VariableValue {
        match variable.type_name.inner() {
            VariableType::Base(_) if variable.memory_location == VariableLocation::Unknown => {
                VariableValue::Empty
            }

            VariableType::Base(type_name) => match type_name.as_str() {
                "!" => VariableValue::Valid("<Never returns>".to_string()),
                "()" => VariableValue::Valid("()".to_string()),
                "bool" => bool::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(err.to_string()),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "char" => char::get_value(variable, memory, variable_cache).into(),
                "i8" => i8::get_value(variable, memory, variable_cache).into(),
                "i16" => i16::get_value(variable, memory, variable_cache).into(),
                "i32" => i32::get_value(variable, memory, variable_cache).into(),
                "i64" => i64::get_value(variable, memory, variable_cache).into(),
                "i128" => i128::get_value(variable, memory, variable_cache).into(),
                "isize" => Self::read_isize(variable, memory, variable_cache),
                "u8" => u8::get_value(variable, memory, variable_cache).into(),
                "u16" => u16::get_value(variable, memory, variable_cache).into(),
                "u32" => u32::get_value(variable, memory, variable_cache).into(),
                "u64" => u64::get_value(variable, memory, variable_cache).into(),
                "u128" => u128::get_value(variable, memory, variable_cache).into(),
                "usize" => Self::read_usize(variable, memory, variable_cache),
                "f32" => f32::get_value(variable, memory, variable_cache)
                    .map(|f| format_float(f as f64))
                    .into(),
                "f64" => f64::get_value(variable, memory, variable_cache)
                    .map(format_float)
                    .into(),
                "None" => VariableValue::Valid("None".to_string()),

                _undetermined_value => VariableValue::Empty,
            },
            VariableType::Struct(name) if name == "&str" => {
                String::get_value(variable, memory, variable_cache).into()
            }
            _other => VariableValue::Empty,
        }
    }

    fn update_variable(
        &self,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match variable.type_name.inner() {
            VariableType::Base(name) => match name.as_str() {
                "bool" => bool::update_value(variable, memory, new_value),
                "char" => char::update_value(variable, memory, new_value),
                "i8" => i8::update_value(variable, memory, new_value),
                "i16" => i16::update_value(variable, memory, new_value),
                "i32" => i32::update_value(variable, memory, new_value),
                "i64" => i64::update_value(variable, memory, new_value),
                "i128" => i128::update_value(variable, memory, new_value),
                "isize" => Self::update_isize(variable, memory, new_value),
                "u8" => u8::update_value(variable, memory, new_value),
                "u16" => u16::update_value(variable, memory, new_value),
                "u32" => u32::update_value(variable, memory, new_value),
                "u64" => u64::update_value(variable, memory, new_value),
                "u128" => u128::update_value(variable, memory, new_value),
                "usize" => Self::update_usize(variable, memory, new_value),
                "f32" => f32::update_value(variable, memory, new_value),
                "f64" => f64::update_value(variable, memory, new_value),
                other => Err(DebugError::WarnAndContinue {
                    message: format!("Updating {other} variables is not yet supported."),
                }),
            },
            other => Err(DebugError::WarnAndContinue {
                message: format!("Updating {} variables is not yet supported.", other.kind()),
            }),
        }
    }

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(format!("{}::{}", type_name.display_name(self), value))
    }

    fn format_array_type(&self, item_type: &str, length: usize) -> String {
        format!("[{item_type}; {length}]")
    }

    fn format_pointer_type(&self, pointee: Option<&str>) -> String {
        let ptr_type = pointee.unwrap_or("<unknown pointer>");

        if ptr_type.starts_with(['*', '&']) {
            ptr_type.to_string()
        } else {
            // FIXME: we should track where the type name came from - the pointer node, or the pointee.
            format!("*raw {ptr_type}")
        }
    }

    fn format_function_name(
        &self,
        function_name: &str,
        function_die: &crate::function_die::FunctionDie<'_>,
        debug_info: &super::DebugInfo,
    ) -> String {
        let parent = function_die.parent_offset();
        if let Some((parent_unit, parent_offset)) = parent
            && let Ok(die) = parent_unit.unit.entry(parent_offset)
            && is_datatype(&die)
            && let Ok(Some(typename)) = parent_unit.extract_type_name(debug_info, &die)
        {
            // TODO: apply better heuristics to clean up the final function name
            if let Some((_, type_generic)) = typename.split_once('<')
                && let Some((function_without_generic, function_generic)) =
                    function_name.split_once('<')
                && type_generic == function_generic
            {
                format!("{typename}::{function_without_generic}")
            } else {
                format!("{typename}::{function_name}")
            }
        } else {
            function_name.to_string()
        }
    }

    fn auto_resolve_children(&self, name: &str) -> bool {
        // Do not match `Some`, `Ok`, or `Err`. `process_variant` already expands the
        // active variant. `starts_with("Err")` also matched `Error` and walked
        // those types on the first expand.
        name.starts_with("&str")
            || name.starts_with("&[")
            || is_rust_type(name, "Option")
            || is_rust_type(name, "Result")
    }

    fn process_struct(
        &self,
        _unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        _node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        if variable.type_name().starts_with("&[") {
            self.expand_slice(debug_info, variable, memory, cache, frame_info)?;
        }

        Ok(())
    }
}

fn is_datatype(entry: &Die) -> bool {
    [gimli::DW_TAG_structure_type, gimli::DW_TAG_enumeration_type].contains(&entry.tag())
}

/// `type_name`, `prefix::type_name`, or those names with a generic argument list.
fn is_rust_type(name: &str, type_name: &str) -> bool {
    fn ident_matches(segment: &str, type_name: &str) -> bool {
        segment == type_name
            || segment
                .strip_prefix(type_name)
                .is_some_and(|tail| tail.starts_with('<'))
    }

    if ident_matches(name, type_name) {
        return true;
    }

    let before_generics = name.split_once('<').map_or(name, |(head, _)| head);
    before_generics
        .rsplit_once("::")
        .is_some_and(|(_, ident)| ident_matches(ident, type_name))
}

#[cfg(test)]
mod tests {
    use super::is_rust_type;

    #[test]
    fn rust_type_name_matches() {
        assert!(is_rust_type("Option", "Option"));
        assert!(is_rust_type(
            "Option<&mut probe_rs_debugger_test::RecursiveStruct>",
            "Option"
        ));
        assert!(is_rust_type("core::option::Option<u32>", "Option"));
        assert!(!is_rust_type("Error", "Err"));
        assert!(!is_rust_type("Optional<u32>", "Option"));
        assert!(is_rust_type("Result<T, E>", "Result"));
    }
}
