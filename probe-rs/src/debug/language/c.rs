use crate::{
    debug::{
        language::ProgrammingLanguage, DebugError, Variable, VariableCache, VariableLocation,
        VariableType, VariableValue,
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

fn read_unsigned_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
) -> Result<String, DebugError> {
    let mut buff = 0usize.to_le_bytes();
    memory.read(
        variable.memory_location.memory_address()?,
        &mut buff[..variable.byte_size.unwrap_or(1) as usize],
    )?;

    Ok(usize::from_le_bytes(buff).to_string())
}

fn read_signed_int(
    variable: &Variable,
    memory: &mut dyn MemoryInterface,
) -> Result<String, DebugError> {
    let mut buff = 0usize.to_le_bytes();
    memory.read(
        variable.memory_location.memory_address()?,
        &mut buff[..variable.byte_size.unwrap_or(1) as usize],
    )?;
    let unsigned = usize::from_le_bytes(buff);
    // sign extend
    let shift = (std::mem::size_of::<isize>() - variable.byte_size.unwrap_or(1) as usize) * 8;
    let signed = (unsigned << shift) as isize >> shift;
    Ok(signed.to_string())
}

fn read_f32(variable: &Variable, memory: &mut dyn MemoryInterface) -> Result<String, DebugError> {
    let mut buff = [0u8; 4];
    memory.read(variable.memory_location.memory_address()?, &mut buff)?;
    Ok(f32::from_le_bytes(buff).to_string())
}
