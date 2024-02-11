use std::str::FromStr;

use crate::{
    debug::{
        language::ProgrammingLanguage, DebugError, Variable, VariableCache, VariableLocation,
        VariableName, VariableType, VariableValue,
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
        _variable_cache: &VariableCache,
    ) -> VariableValue {
        match &variable.type_name {
            VariableType::Base(_) if variable.memory_location == VariableLocation::Unknown => {
                VariableValue::Empty
            }

            VariableType::Base(type_name) => match type_name.as_str() {
                "_Bool" => read_unsigned_int(variable, memory).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    VariableValue::Valid,
                ),

                "char" => read_c_char(variable, memory).map_or_else(
                    |err| VariableValue::Error(format!("{err:?}")),
                    VariableValue::Valid,
                ),

                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    read_unsigned_int(variable, memory).map_or_else(
                        |err| VariableValue::Error(format!("{err:?}")),
                        VariableValue::Valid,
                    )
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => read_signed_int(variable, memory)
                    .map_or_else(
                        |err| VariableValue::Error(format!("{err:?}")),
                        VariableValue::Valid,
                    ),

                "float" => match variable.byte_size {
                    Some(4) | None => read_f32(variable, memory).map_or_else(
                        |err| VariableValue::Error(format!("{err:?}")),
                        VariableValue::Valid,
                    ),
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
                "_Bool" => write_unsigned_int(variable, memory, new_value),
                "char" => write_c_char(variable, memory, new_value),
                "unsigned char" | "unsigned int" | "short unsigned int" | "long unsigned int" => {
                    write_unsigned_int(variable, memory, new_value)
                }
                "signed char" | "int" | "short int" | "long int" | "signed int"
                | "short signed int" | "long signed int" => {
                    write_signed_int(variable, memory, new_value)
                }
                "float" => write_f32(variable, memory, new_value),
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

fn read_c_char(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
) -> Result<String, DebugError> {
    let mut buff = 0u8;
    memory.read(
        variable.memory_location.memory_address()?,
        std::slice::from_mut(&mut buff),
    )?;

    Ok(if buff.is_ascii() {
        (buff as char).to_string()
    } else {
        format!("\\x{:02x}", buff)
    })
}

fn write_c_char(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
    new_value: &str,
) -> Result<(), DebugError> {
    fn input_error(value: &str) -> DebugError {
        DebugError::UnwindIncompleteResults {
            message: format!("Invalid value for char: {value}. Please provide a single character."),
        }
    }

    // TODO: what do we want to support here exactly? This is now symmetrical with read_c_char
    // but we could be somewhat smarter, too.
    let new_value = if new_value.len() == 1 && new_value.is_ascii() {
        new_value.as_bytes()[0]
    } else if new_value.starts_with("\\x") && [3, 4].contains(&new_value.len()) {
        u8::from_str_radix(&new_value[2..], 16).map_err(|_| input_error(new_value))?
    } else {
        return Err(input_error(new_value));
    };

    memory.write(variable.memory_location.memory_address()?, &[new_value])?;

    Ok(())
}

/// A very naive implementation of printing an arbitrary length number.
fn print_arbitrary_length(is_signed: bool, num: &mut [u8]) -> String {
    let prefix = if is_signed {
        let negative = num.last().map_or(false, |&x| x & 0x80 != 0);
        if negative {
            twos_complement(num);
            "-"
        } else {
            ""
        }
    } else {
        ""
    };

    // in a loop, we divide the number by 10 and print the remainder digit
    let mut out = String::new();
    while num.iter().any(|&x| x != 0) {
        let carry = divide(num, 10);
        out.insert(0, char::from_digit(carry, 10).unwrap());
    }

    if out.is_empty() {
        out.push('0');
    }

    out.insert_str(0, prefix);

    out
}

// Divide byte-by-byte
fn divide(num: &mut [u8], by: u8) -> u32 {
    let by = by as u32;

    let mut carry = 0;
    for byte in num.iter_mut().rev() {
        let val = *byte as u32 + carry * 256;
        *byte = (val / by) as u8;
        carry = val % by;
    }

    carry
}

fn twos_complement(num: &mut [u8]) {
    let mut carry = true;
    for byte in num.iter_mut() {
        *byte = !*byte;
        let (new, overflow) = byte.overflowing_add(carry as u8);
        *byte = new;
        carry = overflow;
    }
}

fn read_unsigned_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
) -> Result<String, DebugError> {
    let mut buff = vec![0u8; variable.byte_size.unwrap_or(1) as usize];
    memory.read(variable.memory_location.memory_address()?, &mut buff)?;

    Ok(print_arbitrary_length(false, &mut buff))
}

fn read_signed_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
) -> Result<String, DebugError> {
    let mut buff = vec![0u8; variable.byte_size.unwrap_or(1) as usize];
    memory.read(variable.memory_location.memory_address()?, &mut buff)?;

    Ok(print_arbitrary_length(true, &mut buff))
}

fn write_unsigned_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
    new_value: &str,
) -> Result<(), DebugError> {
    let buff = u128::to_le_bytes(<u128 as FromStr>::from_str(new_value).map_err(|error| {
        DebugError::UnwindIncompleteResults {
            message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
        }
    })?);

    // TODO: check that value actually fits into `bytes` number of bytes
    let bytes = variable.byte_size.unwrap_or(1) as usize;
    memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;

    Ok(())
}

fn write_signed_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
    new_value: &str,
) -> Result<(), DebugError> {
    let buff = i128::to_le_bytes(<i128 as FromStr>::from_str(new_value).map_err(|error| {
        DebugError::UnwindIncompleteResults {
            message: format!("Invalid data conversion from value: {new_value:?}. {error:?}"),
        }
    })?);

    // TODO: check that value actually fits into `bytes` number of bytes
    let bytes = variable.byte_size.unwrap_or(1) as usize;
    memory.write_8(variable.memory_location.memory_address()?, &buff[..bytes])?;

    Ok(())
}

fn read_f32(variable: &Variable, memory: &mut dyn MemoryInterface) -> Result<String, DebugError> {
    let mut buff = [0u8; 4];
    memory.read(variable.memory_location.memory_address()?, &mut buff)?;
    Ok(f32::from_le_bytes(buff).to_string())
}

fn write_f32(
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_print_arbitrary_length_positive() {
        assert_eq!(print_arbitrary_length(true, &mut []), "0");

        let mut buff = [0x78, 0x56, 0x34, 0x12];
        assert_eq!(print_arbitrary_length(false, &mut buff), "305419896");

        let mut buff = [0xFC, 0xFF, 0xFF];
        assert_eq!(print_arbitrary_length(false, &mut buff), "16777212");

        let mut buff = [0x78, 0x56, 0x34, 0x12];
        assert_eq!(print_arbitrary_length(true, &mut buff), "305419896");

        let mut buff = [0xFC, 0xFF, 0xFF];
        assert_eq!(print_arbitrary_length(true, &mut buff), "-4");
    }
}
