use gimli::DwLang;

use crate::{
    debug::{
        Bitfield, DebugError, Modifier, Variable, VariableCache, VariableName, VariableType,
        VariableValue,
    },
    MemoryInterface,
};

/// C, C89, C99, C11, ...
pub mod c;
/// Rust
pub mod rust;

mod parsing;
mod value;

pub fn from_dwarf(language: DwLang) -> Box<dyn ProgrammingLanguage> {
    match language {
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

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue;

    fn format_array_type(&self, item_type: &str, length: usize) -> String;
    fn format_bitfield_type(&self, item_type: &str, bitfield: Bitfield) -> String {
        format!(
            "{item_type} {{{}..{}}}",
            bitfield.normalized_offset(),
            bitfield.normalized_offset() + bitfield.length()
        )
    }
    fn format_pointer_type(&self, pointee: Option<&str>) -> String;

    fn process_tag_with_no_type(&self, _variable: &Variable, tag: gimli::DwTag) -> VariableValue {
        VariableValue::Error(format!("Error: Failed to decode {tag} type reference"))
    }

    fn auto_resolve_children(&self, _name: &str) -> bool {
        false
    }

    fn modified_type_name(&self, modifier: &Modifier, name: &str) -> String {
        match modifier {
            Modifier::Const => format!("const {}", name),
            Modifier::Volatile => format!("volatile {}", name),
            Modifier::Restrict => format!("restrict {}", name),
            Modifier::Atomic => format!("_Atomic {}", name),
            Modifier::Typedef(ty) => ty.to_string(),
        }
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
        Err(DebugError::Other(format!(
            "Updating variables for language {} is not supported.",
            self.0
        )))
    }

    fn format_enum_value(&self, type_name: &VariableType, value: &VariableName) -> VariableValue {
        VariableValue::Valid(format!("{}::{}", type_name.display_name(self), value))
    }

    fn format_array_type(&self, item_type: &str, length: usize) -> String {
        format!("[{item_type}; {length}]")
    }

    fn format_pointer_type(&self, pointee: Option<&str>) -> String {
        pointee.unwrap_or("<unknown pointer>").to_string()
    }
}
