use gimli::{DebuggingInformationEntry, DwLang};
use probe_rs::MemoryInterface;

use crate::{
    Bitfield, DebugError, DebugInfo, GimliReader, Modifier, Variable, VariableCache, VariableName,
    VariableType, VariableValue, function_die::FunctionDie, stack_frame::StackFrameInfo,
    unit_info::UnitInfo,
};

/// C, C89, C99, C11, ...
pub mod c;
/// Rust
pub mod rust;

mod parsing;
mod value;

pub fn from_dwarf(language: DwLang) -> Box<dyn ProgrammingLanguage + Send + Sync> {
    match language {
        // Handle all C-like languages the same now.
        // We may have to split it later if this is not good enough.
        gimli::DW_LANG_C
        | gimli::DW_LANG_C89
        | gimli::DW_LANG_C99
        | gimli::DW_LANG_C11
        | gimli::DW_LANG_C17
        | gimli::DW_LANG_C_plus_plus
        | gimli::DW_LANG_C_plus_plus_03
        | gimli::DW_LANG_C_plus_plus_11
        | gimli::DW_LANG_C_plus_plus_14
        | gimli::DW_LANG_C_plus_plus_17
        | gimli::DW_LANG_C_plus_plus_20 => Box::new(c::C),
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

    fn type_path_separator(&self) -> &str {
        "::"
    }

    fn format_generic_type(&self, ident: &str, args: &[String]) -> String {
        if args.is_empty() {
            ident.to_string()
        } else {
            format!("{ident}<{}>", args.join(", "))
        }
    }

    /// Parse a compiler type name when DWARF has no structured template parameters.
    fn parse_type_name(&self, _name: &str) -> Option<VariableType> {
        None
    }

    /// Language-specific head of a named type, for example `&mut T` or `fn(T)`.
    fn format_named_head(&self, _ident: &str, _args: &[String]) -> Option<String> {
        None
    }

    /// `true` if `ident` is a path segment that may follow a namespace.
    fn is_path_ident(&self, ident: &str) -> bool {
        ident
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '{')
    }

    /// Compact a demangled or DIE function name for stack traces.
    fn compact_debug_name(&self, name: &str) -> String {
        name.to_string()
    }

    fn format_function_name(
        &self,
        function_name: &str,
        _function_die: &FunctionDie<'_>,
        _debug_info: &super::DebugInfo,
    ) -> String {
        self.compact_debug_name(function_name)
    }

    fn process_tag_with_no_type(&self, _variable: &Variable, tag: gimli::DwTag) -> VariableValue {
        VariableValue::Error(format!("Error: Failed to decode {tag} type reference"))
    }

    fn auto_resolve_children(&self, _ty: &VariableType) -> bool {
        false
    }

    /// Report whether a read of the members of `ty` can change the state of the target, because
    /// the members are memory mapped device registers. Such a type only expands when the client
    /// asks for it.
    fn is_side_effecting(&self, _ty: &VariableType) -> bool {
        false
    }

    fn modified_type_name(&self, modifier: &Modifier, name: &str) -> String {
        match modifier {
            Modifier::Const => format!("const {name}"),
            Modifier::Volatile => format!("volatile {name}"),
            Modifier::Restrict => format!("restrict {name}"),
            Modifier::Atomic => format!("_Atomic {name}"),
            Modifier::Typedef(ty) => ty.to_string(),
        }
    }

    // Post-process raw type representations for more user-friendly output.
    #[expect(clippy::too_many_arguments)]
    fn process_struct(
        &self,
        _unit_info: &UnitInfo,
        _debug_info: &DebugInfo,
        _node: &DebuggingInformationEntry<GimliReader>,
        _variable: &mut Variable,
        _memory: &mut dyn MemoryInterface,
        _cache: &mut VariableCache,
        _frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        Ok(())
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
        VariableValue::Valid(format!(
            "{}::{}",
            type_name.display_name_with_style(self, crate::TypeNameStyle::Compact),
            value
        ))
    }

    fn format_array_type(&self, item_type: &str, length: usize) -> String {
        format!("[{item_type}; {length}]")
    }

    fn format_pointer_type(&self, pointee: Option<&str>) -> String {
        pointee.unwrap_or("<unknown pointer>").to_string()
    }
}
