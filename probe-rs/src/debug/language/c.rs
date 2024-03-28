use crate::{
    debug::{
        language::{
            value::{format_float, Value},
            ProgrammingLanguage,
        },
        Bitfield, DebugError, Variable, VariableCache, VariableLocation, VariableName,
        VariableType, VariableValue,
    },
    MemoryInterface,
};
use std::fmt::{Display, Write};

#[derive(Debug, Clone)]
pub struct C;

impl ProgrammingLanguage for C {
    fn read_variable_value(
        &self,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> VariableValue {
        match variable.type_name.inner() {
            _ if variable.memory_location == VariableLocation::Unknown => VariableValue::Empty,

            VariableType::Base(name) => match name.as_str() {
                "_Bool" => UnsignedInt::get_value(variable, None, memory, variable_cache).into(),
                "char" => CChar::get_value(variable, memory, variable_cache).into(),

                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    UnsignedInt::get_value(variable, None, memory, variable_cache).into()
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => {
                    SignedInt::get_value(variable, None, memory, variable_cache).into()
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

            VariableType::Bitfield(bitfield, name) => match name.inner() {
                VariableType::Base(name) => {
                    if name.as_str().contains("unsigned") {
                        UnsignedInt::get_value(variable, Some(*bitfield), memory, variable_cache)
                            .into()
                    } else {
                        SignedInt::get_value(variable, Some(*bitfield), memory, variable_cache)
                            .into()
                    }
                }

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
        match variable.type_name.inner() {
            VariableType::Base(name) => match name.as_str() {
                "_Bool" => UnsignedInt::update_value(variable, None, memory, new_value),
                "char" => CChar::update_value(variable, memory, new_value),
                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    UnsignedInt::update_value(variable, None, memory, new_value)
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => {
                    SignedInt::update_value(variable, None, memory, new_value)
                }
                "float" => f32::update_value(variable, memory, new_value),
                // TODO: doubles
                other => Err(DebugError::WarnAndContinue {
                    message: format!("Updating {other} variables is not yet supported."),
                }),
            },

            VariableType::Bitfield(bitfield, name) => match name.inner() {
                VariableType::Base(name) => {
                    if name.as_str().contains("unsigned") {
                        UnsignedInt::update_value(variable, Some(*bitfield), memory, new_value)
                    } else {
                        SignedInt::update_value(variable, Some(*bitfield), memory, new_value)
                    }
                }

                other => Err(DebugError::WarnAndContinue {
                    message: format!(
                        "Updating {} bitfield variables is not yet supported.",
                        other.kind()
                    ),
                }),
            },

            other => Err(DebugError::WarnAndContinue {
                message: format!("Updating {} variables is not yet supported.", other.kind()),
            }),
        }
    }

    fn format_array_type(&self, item_type: &str, length: usize) -> String {
        format!("{item_type}[{length}]")
    }

    fn format_enum_value(&self, _type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(value.to_string())
    }

    fn format_pointer_type(&self, pointee: Option<&str>) -> String {
        format!("{}*", pointee.unwrap_or("void"))
    }

    fn process_tag_with_no_type(&self, variable: &Variable, tag: gimli::DwTag) -> VariableValue {
        match tag {
            gimli::DW_TAG_const_type => VariableValue::Valid("const void".to_string()),
            gimli::DW_TAG_pointer_type => {
                let name = if let VariableLocation::Address(addr) = variable.memory_location {
                    format!("void* @ {addr:X}")
                } else {
                    "void*".to_string()
                };

                VariableValue::Valid(name)
            }
            _ => VariableValue::Error(format!("Error: Failed to decode {tag} type reference")),
        }
    }
}

struct CChar(u8);

impl Display for CChar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let c = self.0;
        if c.is_ascii() {
            f.write_char(c as char)
        } else {
            f.write_fmt(format_args!("\\x{:02x}", c))
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
            DebugError::WarnAndContinue {
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

impl Display for UnsignedInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0))
    }
}

impl UnsignedInt {
    fn get_value(
        variable: &Variable,
        bitfield: Option<Bitfield>,
        memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized,
    {
        // Read the bits
        let mut buff = [0u8; 16];
        let bytes = variable.byte_size.unwrap_or(1).min(16) as usize;
        memory.read(
            variable.memory_location.memory_address()?,
            &mut buff[..bytes],
        )?;
        let value = u128::from_le_bytes(buff);

        // Extract bitfield bits
        let value = match bitfield {
            Some(bitfield) => bitfield.extract(value),
            None => value,
        };

        Ok(Self(value))
    }

    fn update_value(
        variable: &Variable,
        bitfield: Option<Bitfield>,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match parse_int::parse::<u128>(new_value) {
            Ok(value) => write_unsigned_bytes(variable, bitfield, memory, value),
            Err(e) => Err(DebugError::WarnAndContinue {
                message: format!("Invalid data conversion from value: {new_value:?}. {e:?}"),
            }),
        }
    }
}

fn write_unsigned_bytes(
    variable: &Variable,
    bitfield: Option<Bitfield>,
    memory: &mut dyn MemoryInterface,
    unsigned: u128,
) -> Result<(), DebugError> {
    // TODO: check that value actually fits into `bytes` number of bytes
    let bytes = variable.byte_size.unwrap_or(1) as usize;

    // Figure out the bitfield offset & bit count
    let Some(bitfield) = bitfield else {
        let buff = unsigned.to_le_bytes();
        memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;
        return Ok(());
    };

    // We are writing a bitfield, we need to do a read-modify-write operation.

    // Read the bits
    let mut buff = [0u8; 16];
    memory.read(
        variable.memory_location.memory_address()?,
        &mut buff[..bytes],
    )?;
    let read_value = u128::from_le_bytes(buff);

    let new_value = bitfield.insert(read_value, unsigned);

    // Write the new value
    let buff = new_value.to_le_bytes();
    memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;

    Ok(())
}

struct SignedInt(i128);

impl Display for SignedInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_fmt(format_args!("{}", self.0))
    }
}

impl SignedInt {
    fn get_value(
        variable: &Variable,
        bitfield: Option<Bitfield>,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized,
    {
        // We read the number as Unsigned first, to avoid duplicating the bitfield handling.
        let unsigned = UnsignedInt::get_value(variable, bitfield, memory, variable_cache)?.0;
        let bytes = variable.byte_size.unwrap_or(1).min(16) as usize;

        // Sign extend
        let sign_bit_shift = match bitfield {
            Some(bitfield) => bitfield.length() as usize - 1,
            None => (bytes * 8) - 1,
        };

        let negative = unsigned & (1 << sign_bit_shift) != 0;
        let value = if negative {
            (unsigned | (!0 << sign_bit_shift)) as i128
        } else {
            unsigned as i128
        };

        Ok(Self(value))
    }

    fn update_value(
        variable: &Variable,
        bitfield: Option<Bitfield>,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match parse_int::parse::<i128>(new_value) {
            Ok(value) => write_unsigned_bytes(variable, bitfield, memory, value as u128),
            Err(e) => Err(DebugError::WarnAndContinue {
                message: format!("Invalid data conversion from value: {new_value:?}. {e:?}"),
            }),
        }
    }
}

impl From<Result<UnsignedInt, DebugError>> for VariableValue {
    fn from(val: Result<UnsignedInt, DebugError>) -> Self {
        val.map_or_else(
            |err| VariableValue::Error(format!("{err:?}")),
            |value| VariableValue::Valid(value.to_string()),
        )
    }
}

impl From<Result<SignedInt, DebugError>> for VariableValue {
    fn from(val: Result<SignedInt, DebugError>) -> Self {
        val.map_or_else(
            |err| VariableValue::Error(format!("{err:?}")),
            |value| VariableValue::Valid(value.to_string()),
        )
    }
}
