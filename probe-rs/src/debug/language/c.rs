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

                _undetermined_value => VariableValue::Empty,
            },
            _other => VariableValue::Empty,
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

/// A very naive implementation of printing an arbitrary length number.
fn print_arbitrary_length(is_signed: bool, num: &mut [u8]) -> String {
    let prefix = if is_signed {
        let negative = num.last().map_or(false, |&x| x & 0x80 != 0);

        if negative {
            // Two's complement
            let mut carry = true;
            for byte in num.iter_mut() {
                *byte = !*byte;
                let (new, overflow) = byte.overflowing_add(carry as u8);
                *byte = new;
                carry = overflow;
            }
        }
        if negative {
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
        // divide byte-by-byte by 10.
        // We could divide by 100 but that way we may end up with a leading 0 we have to remove
        let mut carry = 0;
        for byte in num.iter_mut().rev() {
            let val = *byte as u32 + carry * 256;
            *byte = (val / 10) as u8;
            carry = val % 10;
        }
        out.insert(0, char::from_digit(carry, 10).unwrap());
    }

    if out.is_empty() {
        out.push('0');
    }

    out.insert_str(0, prefix);

    out
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

fn read_f32(variable: &Variable, memory: &mut dyn MemoryInterface) -> Result<String, DebugError> {
    let mut buff = [0u8; 4];
    memory.read(variable.memory_location.memory_address()?, &mut buff)?;
    Ok(f32::from_le_bytes(buff).to_string())
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
