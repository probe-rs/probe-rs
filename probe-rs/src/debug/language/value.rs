use std::str::FromStr;

use crate::{
    debug::{
        language::parsing::ParseToBytes, DebugError, Variable, VariableCache, VariableName,
        VariableValue,
    },
    MemoryInterface,
};

/// Traits and Impl's to read from, and write to, memory value based on Variable::typ and Variable::location.
pub trait Value {
    /// The MS DAP protocol passes the value as a string, so this trait is here to provide the memory read logic before returning it as a string.
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized;

    /// This `update_value` will update the target memory with a new value for the [`Variable`], ...
    /// - Only `base` data types can have their value updated in target memory.
    /// - The input format of the [Variable.value] is a [String], and the impl of this trait must convert the memory value appropriately before storing.
    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError>;
}

impl<V> From<Result<V, DebugError>> for VariableValue
where
    V: Value + ToString,
{
    fn from(val: Result<V, DebugError>) -> Self {
        val.map_or_else(
            |err| VariableValue::Error(format!("{err:?}")),
            |value| VariableValue::Valid(value.to_string()),
        )
    }
}

impl Value for bool {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mem_data = memory.read_word_8(variable.memory_location.memory_address()?)?;
        let ret_value: bool = mem_data != 0;
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        memory
            .write_word_8(
                variable.memory_location.memory_address()?,
                <bool as FromStr>::from_str(new_value).map_err(|error| {
                    DebugError::WarnAndContinue {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })? as u8,
            )
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for char {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mem_data = memory.read_word_32(variable.memory_location.memory_address()?)?;
        if let Some(return_value) = char::from_u32(mem_data) {
            Ok(return_value)
        } else {
            Ok('?')
        }
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        memory
            .write_word_32(
                variable.memory_location.memory_address()?,
                <char as FromStr>::from_str(new_value).map_err(|error| {
                    DebugError::WarnAndContinue {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })? as u32,
            )
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for String {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut str_value: String = "".to_owned();
        let children: Vec<_> = variable_cache.get_children(variable.variable_key).collect();
        if !children.is_empty() {
            let mut string_length = match children.iter().find(|child_variable| {
                    matches!(child_variable.name, VariableName::Named(ref name) if name == "length")
                }) {
                    Some(string_length) => {
                        // TODO: maybe avoid accessing value directly?
                        if let VariableValue::Valid(length_value) = &string_length.value {
                            length_value.parse().unwrap_or(0_usize)
                        } else {
                            0_usize
                        }
                    }
                    None => 0_usize,
                };

            let string_location = match children.iter().find(|child_variable| {
                    matches!(child_variable.name, VariableName::Named(ref name) if name == "data_ptr")
                }) {
                    Some(location_value) => {
                        let mut child_variables = variable_cache.get_children(location_value.variable_key);
                        if let Some(first_child) = child_variables.next() {
                            first_child.memory_location.memory_address()?
                        } else {
                            0_u64
                        }
                    }
                    None => 0_u64,
                };
            if string_location == 0 {
                str_value = "Error: Failed to determine &str memory location".to_string();
            } else {
                // Limit string length to work around buggy information, otherwise the debugger
                // can hang due to buggy debug information.
                //
                // TODO: If implemented, the variable should not be fetched automatically,
                // but only when requested by the user. This workaround can then be removed.
                if string_length > 200 {
                    tracing::warn!(
                        "Very long string ({} bytes), truncating to 200 bytes.",
                        string_length
                    );
                    string_length = 200;
                }

                if string_length == 0 {
                    // A string with length 0 doesn't need to be read from memory.
                } else {
                    let mut buff = vec![0u8; string_length];
                    memory.read(string_location, &mut buff)?;
                    std::str::from_utf8(&buff)?.clone_into(&mut str_value);
                }
            }
        } else {
            str_value = "Error: Failed to evaluate &str value".to_string();
        }
        Ok(str_value)
    }

    fn update_value(
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _new_value: &str,
    ) -> Result<(), DebugError> {
        Err(DebugError::WarnAndContinue { message:"Unsupported datatype: \"String\". Please only update variables with a base data type.".to_string()})
    }
}
impl Value for i8 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = i8::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i8::parse_to_bytes(new_value)?;
        memory
            .write_word_8(variable.memory_location.memory_address()?, buff[0])
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for i16 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = i16::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i16::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for i32 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i32::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for i64 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = i64::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i64::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for i128 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = i128::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i128::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for u8 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = u8::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u8::parse_to_bytes(new_value)?;
        memory
            .write_word_8(variable.memory_location.memory_address()?, buff[0])
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for u16 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = u16::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u16::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for u32 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u32::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for u64 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = u64::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u64::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for u128 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = u128::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u128::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for f32 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = f32::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = f32::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}
impl Value for f64 {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        let ret_value = f64::from_le_bytes(buff);
        Ok(ret_value)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = f64::parse_to_bytes(new_value)?;
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::WarnAndContinue {
                message: format!("{error:?}"),
            })
    }
}

/// Format a float value to a string, preserving at least one fractional digit.
pub fn format_float(value: f64) -> String {
    let mut s = format!("{}", value);
    if !s.contains('.') {
        s.push('.');
    }
    while s.ends_with('0') {
        s.pop();
    }
    if s.ends_with('.') {
        s.push('0');
    }

    s
}
