use gimli::DwLang;

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
}

#[derive(Clone)]
pub struct UnknownLanguage;

impl ProgrammingLanguage for UnknownLanguage {}
