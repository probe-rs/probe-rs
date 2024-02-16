use crate::{
    debug::{
        language::{
            parsing::ParseToBytes,
            value::{format_float, Value},
            ProgrammingLanguage,
        },
        DebugError, Variable, VariableCache, VariableLocation, VariableName, VariableType,
        VariableValue,
    },
    MemoryInterface,
};

#[derive(Clone)]
pub struct C;

impl ProgrammingLanguage for C {
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
                "_Bool" => UnsignedInt::get_value(variable, memory, variable_cache).into(),
                "char" => CChar::get_value(variable, memory, variable_cache).into(),

                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    UnsignedInt::get_value(variable, memory, variable_cache).into()
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => {
                    SignedInt::get_value(variable, memory, variable_cache).into()
                }

                "float" => match variable.byte_size {
                    Some(4) | None => f32::get_value(variable, memory, variable_cache)
                        .map(|f| format_float(f as f64))
                        .into(),
                    Some(size) => {
                        VariableValue::Error(format!("Invalid byte size for float: {size}"))
                    }
                },
                // TODO: doubles
                _undetermined_value => VariableValue::Empty,
            },
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
                "_Bool" => UnsignedInt::update_value(variable, memory, new_value),
                "char" => CChar::update_value(variable, memory, new_value),
                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    UnsignedInt::update_value(variable, memory, new_value)
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => {
                    SignedInt::update_value(variable, memory, new_value)
                }
                "float" => f32::update_value(variable, memory, new_value),
                // TODO: doubles
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

    fn format_enum_value(&self, _type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(value.to_string())
    }

    fn process_tag_with_no_type(&self, tag: gimli::DwTag) -> VariableValue {
        match tag {
            gimli::DW_TAG_const_type => VariableValue::Valid("<void>".to_string()),
            _ => VariableValue::Error(format!("Error: Failed to decode {tag} type reference")),
        }
    }
}

struct CChar(u8);

impl ToString for CChar {
    fn to_string(&self) -> String {
        let c = self.0;
        if c.is_ascii() {
            (c as char).to_string()
        } else {
            format!("\\x{:02x}", c)
        }
    }
}

impl Value for CChar {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized,
    {
        let mut buff = 0u8;
        memory.read(
            variable.memory_location.memory_address()?,
            std::slice::from_mut(&mut buff),
        )?;

        Ok(Self(buff))
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        fn input_error(value: &str) -> DebugError {
            DebugError::UnwindIncompleteResults {
                message: format!(
                    "Invalid value for char: {value}. Please provide a single character."
                ),
            }
        }

        // TODO: what do we want to support here exactly? This is now symmetrical with get_value
        // but we could be somewhat smarter, too.
        let new_value = if new_value.len() == 1 && new_value.is_ascii() {
            new_value.as_bytes()[0]
        } else if new_value.starts_with("\\x") && [3, 4].contains(&new_value.len()) {
            u8::from_str_radix(&new_value[2..], 16).map_err(|_| input_error(new_value))?
        } else {
            return Err(input_error(new_value));
        };

        memory.write_word_8(variable.memory_location.memory_address()?, new_value)?;

        Ok(())
    }
}

struct UnsignedInt(u128);

impl ToString for UnsignedInt {
    fn to_string(&self) -> String {
        self.0.to_string()
    }
}

impl Value for UnsignedInt {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized,
    {
        let mut buff = [0u8; 16];
        let bytes = variable.byte_size.unwrap_or(1).min(16) as usize;
        memory.read(
            variable.memory_location.memory_address()?,
            &mut buff[..bytes],
        )?;

        Ok(Self(u128::from_le_bytes(buff)))
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = u128::parse_to_bytes(new_value)?;

        // TODO: check that value actually fits into `bytes` number of bytes
        let bytes = variable.byte_size.unwrap_or(1) as usize;
        memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;

        Ok(())
    }
}

struct SignedInt(i128);

impl ToString for SignedInt {
    fn to_string(&self) -> String {
        self.0.to_string()
    }
}

impl Value for SignedInt {
    fn get_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized,
    {
        let mut buff = [0u8; 16];
        let bytes = variable.byte_size.unwrap_or(1).min(16) as usize;
        memory.read(
            variable.memory_location.memory_address()?,
            &mut buff[..bytes],
        )?;

        // sign extend
        let negative = buff[bytes - 1] >= 0x80;
        if negative {
            buff[bytes..].fill(0xFF);
        }

        Ok(Self(i128::from_le_bytes(buff)))
    }

    fn update_value(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        let buff = i128::parse_to_bytes(new_value)?;

        // TODO: check that value actually fits into `bytes` number of bytes
        let bytes = variable.byte_size.unwrap_or(1) as usize;
        memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;

        Ok(())
    }
}
