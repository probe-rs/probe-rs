use std::str::FromStr;

use crate::{
    debug::{
        language::ProgrammingLanguage, DebugError, Variable, VariableCache, VariableLocation,
        VariableName, VariableType, VariableValue,
    },
    MemoryInterface,
};

#[derive(Clone)]
pub struct Rust;

impl ProgrammingLanguage for Rust {
    fn read_variable_value(
        &self,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> VariableValue {
        match &variable.type_name {
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
                "char" => char::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "i8" => i8::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "i16" => i16::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "i32" => i32::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "i64" => i64::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "i128" => i128::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "isize" => isize::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "u8" => u8::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "u16" => u16::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "u32" => u32::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "u64" => u64::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "u128" => u128::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "usize" => usize::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "f32" => f32::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "f64" => f64::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "None" => VariableValue::Valid("None".to_string()),

                _undetermined_value => VariableValue::Empty,
            },
            VariableType::Struct(name) if name == "&str" => {
                String::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    VariableValue::Valid,
                )
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
        match &variable.type_name {
            VariableType::Base(name) => match name.as_str() {
                "bool" => bool::update_value(variable, memory, new_value),
                "char" => char::update_value(variable, memory, new_value),
                "i8" => i8::update_value(variable, memory, new_value),
                "i16" => i16::update_value(variable, memory, new_value),
                "i32" => i32::update_value(variable, memory, new_value),
                "i64" => i64::update_value(variable, memory, new_value),
                "i128" => i128::update_value(variable, memory, new_value),
                "isize" => isize::update_value(variable, memory, new_value),
                "u8" => u8::update_value(variable, memory, new_value),
                "u16" => u16::update_value(variable, memory, new_value),
                "u32" => u32::update_value(variable, memory, new_value),
                "u64" => u64::update_value(variable, memory, new_value),
                "u128" => u128::update_value(variable, memory, new_value),
                "usize" => usize::update_value(variable, memory, new_value),
                "f32" => f32::update_value(variable, memory, new_value),
                "f64" => f64::update_value(variable, memory, new_value),
                other => Err(DebugError::UnwindIncompleteResults {
                    message: format!("Unsupported data type: {other}. Please only update variables with a base data type."),
                }),
            },
            other => Err(DebugError::UnwindIncompleteResults {
                message: format!(
                    "Unsupported variable type {:?}. Only base variables can be updated.",
                    other
                ),
            }),
        }
    }

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(format!("{}::{}", type_name, value))
    }
}

/// Traits and Impl's to read from, and write to, memory value based on Variable::typ and Variable::location.
trait Value {
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
                    DebugError::UnwindIncompleteResults {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })? as u8,
            )
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
                    DebugError::UnwindIncompleteResults {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })? as u32,
            )
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        if let Ok(children) = variable_cache.get_children(variable.variable_key) {
            if !children.is_empty() {
                let mut string_length = match children.iter().find(|child_variable| {
                    child_variable.name == VariableName::Named("length".to_string())
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
                    child_variable.name == VariableName::Named("data_ptr".to_string())
                }) {
                    Some(location_value) => {
                        if let Ok(child_variables) =
                            variable_cache.get_children(location_value.variable_key)
                        {
                            if let Some(first_child) = child_variables.first() {
                                first_child.memory_location.memory_address()?
                            } else {
                                0_u64
                            }
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
                        str_value = core::str::from_utf8(&buff)?.to_owned();
                    }
                }
            } else {
                str_value = "Error: Failed to evaluate &str value".to_string();
            }
        };
        Ok(str_value)
    }

    fn update_value(
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _new_value: &str,
    ) -> Result<(), DebugError> {
        Err(DebugError::UnwindIncompleteResults { message:"Unsupported datatype: \"String\". Please only update variables with a base data type.".to_string()})
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
        memory
            .write_word_8(
                variable.memory_location.memory_address()?,
                <i8 as FromStr>::from_str(new_value).map_err(|error| {
                    DebugError::UnwindIncompleteResults {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })? as u8,
            )
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = i16::to_le_bytes(<i16 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = i32::to_le_bytes(<i32 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = i64::to_le_bytes(<i64 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = i128::to_le_bytes(<i128 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
                message: format!("{error:?}"),
            })
    }
}
impl Value for isize {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        // TODO: We can get the actual WORD length from [DWARF] instead of assuming `u32`
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value as isize)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff =
            isize::to_le_bytes(<isize as FromStr>::from_str(new_value).map_err(|error| {
                DebugError::UnwindIncompleteResults {
                    message: format!(
                        "Invalid data conversion from value: {new_value:?}. {error:?}"
                    ),
                }
            })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        memory
            .write_word_8(
                variable.memory_location.memory_address()?,
                <u8 as FromStr>::from_str(new_value).map_err(|error| {
                    DebugError::UnwindIncompleteResults {
                        message: format!(
                            "Invalid data conversion from value: {new_value:?}. {error:?}"
                        ),
                    }
                })?,
            )
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = u16::to_le_bytes(<u16 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = u32::to_le_bytes(<u32 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = u64::to_le_bytes(<u64 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = u128::to_le_bytes(<u128 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
                message: format!("{error:?}"),
            })
    }
}
impl Value for usize {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        memory.read(variable.memory_location.memory_address()?, &mut buff)?;
        // TODO: We can get the actual WORD length from [DWARF] instead of assuming `u32`
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value as usize)
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff =
            usize::to_le_bytes(<usize as FromStr>::from_str(new_value).map_err(|error| {
                DebugError::UnwindIncompleteResults {
                    message: format!(
                        "Invalid data conversion from value: {new_value:?}. {error:?}"
                    ),
                }
            })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = f32::to_le_bytes(<f32 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
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
        let buff = f64::to_le_bytes(<f64 as FromStr>::from_str(new_value).map_err(|error| {
            DebugError::UnwindIncompleteResults {
                message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
            }
        })?);
        memory
            .write_8(variable.memory_location.memory_address()?, &buff)
            .map_err(|error| DebugError::UnwindIncompleteResults {
                message: format!("{error:?}"),
            })
    }
}
