use crate::{
    debug::{
        language::{
            value::{format_float, Value},
            ProgrammingLanguage,
        },
        DebugError, Variable, VariableCache, VariableLocation, VariableName, VariableType,
        VariableValue,
    },
    MemoryInterface,
};

#[derive(Debug, Clone)]
pub struct Rust;

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
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "char" => char::get_value(variable, memory, variable_cache).into(),
                "i8" => i8::get_value(variable, memory, variable_cache).into(),
                "i16" => i16::get_value(variable, memory, variable_cache).into(),
                "i32" => i32::get_value(variable, memory, variable_cache).into(),
                "i64" => i64::get_value(variable, memory, variable_cache).into(),
                "i128" => i128::get_value(variable, memory, variable_cache).into(),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `i32`
                "isize" => i32::get_value(variable, memory, variable_cache).into(),
                "u8" => u8::get_value(variable, memory, variable_cache).into(),
                "u16" => u16::get_value(variable, memory, variable_cache).into(),
                "u32" => u32::get_value(variable, memory, variable_cache).into(),
                "u64" => u64::get_value(variable, memory, variable_cache).into(),
                "u128" => u128::get_value(variable, memory, variable_cache).into(),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `u32`
                "usize" => u32::get_value(variable, memory, variable_cache).into(),
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
                // TODO: We can get the actual WORD length from DWARF instead of assuming `i32`
                "isize" => i32::update_value(variable, memory, new_value),
                "u8" => u8::update_value(variable, memory, new_value),
                "u16" => u16::update_value(variable, memory, new_value),
                "u32" => u32::update_value(variable, memory, new_value),
                "u64" => u64::update_value(variable, memory, new_value),
                "u128" => u128::update_value(variable, memory, new_value),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `u32`
                "usize" => u32::update_value(variable, memory, new_value),
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
            format!("*raw {}", ptr_type)
        }
    }

    fn auto_resolve_children(&self, name: &str) -> bool {
        name.starts_with("&str")
            || name.starts_with("Option")
            || name.starts_with("Some")
            || name.starts_with("Result")
            || name.starts_with("Ok")
            || name.starts_with("Err")
    }
}
