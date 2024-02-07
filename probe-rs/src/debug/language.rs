use gimli::DwLang;

use crate::{
    debug::{DebugError, Variable, VariableCache, VariableValue},
    MemoryInterface,
};

/// C, C89, C99, C11
pub mod c;
/// Rust
pub mod rust;

pub fn from_dwarf(dwarf_language: DwLang) -> Box<dyn ProgrammingLanguage> {
    match dwarf_language {
        gimli::DW_LANG_C => Box::new(c::C),
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
}

#[derive(Clone)]
pub struct UnknownLanguage;

impl ProgrammingLanguage for UnknownLanguage {}
