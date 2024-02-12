use gimli::DwLang;
use num_traits::Num;

use crate::{
    debug::{DebugError, Variable, VariableCache, VariableName, VariableType, VariableValue},
    MemoryInterface,
};

/// C, C89, C99, C11, ...
pub mod c;
/// Rust
pub mod rust;

pub fn from_dwarf(dwarf_language: DwLang) -> Box<dyn ProgrammingLanguage> {
    match dwarf_language {
        // Handle all C-like languages the same now.
        // We may have to split it later if this is not good enough.
        gimli::DW_LANG_C
        | gimli::DW_LANG_C89
        | gimli::DW_LANG_C99
        | gimli::DW_LANG_C11
        | gimli::DW_LANG_C17 => Box::new(c::C),
        gimli::DW_LANG_Rust => Box::new(rust::Rust),
        _ => Box::new(UnknownLanguage),
    }
}

/// Programming language specific operations.
pub trait ProgrammingLanguage {
    fn read_variable_value(
        &self,
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> VariableValue {
        VariableValue::Empty
    }

    fn update_variable(
        &self,
        variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _new_value: &str,
    ) -> Result<(), DebugError> {
        Err(DebugError::UnwindIncompleteResults {
            message: format!(
                "Unsupported variable type {:?}. Only base variables can be updated.",
                variable.type_name
            ),
        })
    }

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(format!("{}::{}", type_name, value))
    }

    fn process_tag_with_no_type(&self, tag: gimli::DwTag) -> VariableValue {
        VariableValue::Error(format!("Error: Failed to decode {tag} type reference"))
    }
}

#[derive(Clone)]
pub struct UnknownLanguage;

impl ProgrammingLanguage for UnknownLanguage {}

/// Extension methods to simply parse a value into a number of bytes.
trait ParseToBytes: Num {
    type Out;

    fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError>;
}

macro_rules! impl_parse {
    ($t:ty, $bytes:expr) => {
        impl ParseToBytes for $t
        where
            <$t as Num>::FromStrRadixErr: ::std::fmt::Debug,
        {
            type Out = [u8; $bytes];

            fn parse_to_bytes(s: &str) -> Result<Self::Out, DebugError> {
                match ::parse_int::parse::<$t>(s) {
                    Ok(value) => Ok(<$t>::to_le_bytes(value)),
                    Err(e) => Err(DebugError::UnwindIncompleteResults {
                        message: format!("Invalid data conversion from value: {s:?}. {e:?}"),
                    }),
                }
            }
        }
    };
}

impl_parse!(u8, 1);
impl_parse!(i8, 1);
impl_parse!(u16, 2);
impl_parse!(i16, 2);
impl_parse!(u32, 4);
impl_parse!(i32, 4);
impl_parse!(u64, 8);
impl_parse!(i64, 8);
impl_parse!(u128, 16);
impl_parse!(i128, 16);

impl_parse!(f32, 4);
impl_parse!(f64, 8);
