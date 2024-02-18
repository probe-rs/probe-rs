use gimli::DwLang;

use crate::{
    debug::{DebugError, Variable, VariableCache, VariableName, VariableType, VariableValue},
    MemoryInterface,
};

/// C, C89, C99, C11, ...
pub mod c;
/// Rust
pub mod rust;

mod parsing;
mod value;

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
        other => Box::new(UnknownLanguage(other)),
    }
}

/// Programming language specific operations.
pub trait ProgrammingLanguage {
    fn read_variable_value(
        &self,
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> VariableValue;

    fn update_variable(
        &self,
        variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _new_value: &str,
    ) -> Result<(), DebugError>;

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(format!("{}::{}", type_name, value))
    }

    fn process_tag_with_no_type(&self, tag: gimli::DwTag) -> VariableValue {
        VariableValue::Error(format!("Error: Failed to decode {tag} type reference"))
    }

    fn auto_resolve_children(&self, _name: &str) -> bool {
        false
    }
}

#[derive(Clone)]
pub struct UnknownLanguage(DwLang);

impl ProgrammingLanguage for UnknownLanguage {
    fn read_variable_value(
        &self,
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _variable_cache: &VariableCache,
    ) -> VariableValue {
        VariableValue::Error(format!(
            "Reading variables for language {} is not supported.",
            self.0
        ))
    }

    fn update_variable(
        &self,
        _variable: &Variable,
        _memory: &mut dyn MemoryInterface,
        _new_value: &str,
    ) -> Result<(), DebugError> {
        Err(DebugError::Other(anyhow::anyhow!(
            "Updating variables for language {} is not supported.",
            self.0
        )))
    }
}
