mod simplify_types;
mod type_name;

use crate::{
    DebugError, DebugInfo, GimliReader, Variable, VariableCache, VariableLocation, VariableName,
    VariableType, VariableValue,
    function_die::Die,
    language::{
        ProgrammingLanguage,
        value::{Value, format_float},
    },
    stack_frame::StackFrameInfo,
    unit_info::UnitInfo,
};

use gimli::DebuggingInformationEntry;
use probe_rs::MemoryInterface;

struct Slice<'a> {
    length: u64,
    data_ptr: &'a Variable,
}

#[derive(Debug, Clone)]
pub struct Rust;
impl Rust {
    fn try_get_slice<'a>(variable: &'a Variable, cache: &'a VariableCache) -> Option<Slice<'a>> {
        fn is_field(var: &Variable, name: &str) -> bool {
            is_named(var, name)
        }

        Some(Slice {
            // Do we have a length?
            length: cache
                .get_children(variable.variable_key)
                .find(|c| is_field(c, "length"))
                .and_then(|field| match &field.value {
                    VariableValue::Valid(length_value) => Some(length_value),
                    _ => None,
                })
                .and_then(|length_str| length_str.parse().ok())?,

            // Do we have a data pointer?
            data_ptr: cache
                .get_children(variable.variable_key)
                .find(|c| is_field(c, "data_ptr"))?,
        })
    }

    /// Reads a `usize`, whose width is the pointer width of the target.
    fn read_usize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &VariableCache,
    ) -> VariableValue {
        match variable.byte_size {
            Some(1) => u8::get_value(variable, memory, cache).into(),
            Some(2) => u16::get_value(variable, memory, cache).into(),
            Some(8) => u64::get_value(variable, memory, cache).into(),
            Some(16) => u128::get_value(variable, memory, cache).into(),
            _ => u32::get_value(variable, memory, cache).into(),
        }
    }

    /// Reads an `isize`, whose width is the pointer width of the target.
    fn read_isize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        cache: &VariableCache,
    ) -> VariableValue {
        match variable.byte_size {
            Some(1) => i8::get_value(variable, memory, cache).into(),
            Some(2) => i16::get_value(variable, memory, cache).into(),
            Some(8) => i64::get_value(variable, memory, cache).into(),
            Some(16) => i128::get_value(variable, memory, cache).into(),
            _ => i32::get_value(variable, memory, cache).into(),
        }
    }

    /// Writes a `usize`, whose width is the pointer width of the target.
    fn update_usize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match variable.byte_size {
            Some(1) => u8::update_value(variable, memory, new_value),
            Some(2) => u16::update_value(variable, memory, new_value),
            Some(8) => u64::update_value(variable, memory, new_value),
            Some(16) => u128::update_value(variable, memory, new_value),
            _ => u32::update_value(variable, memory, new_value),
        }
    }

    /// Writes an `isize`, whose width is the pointer width of the target.
    fn update_isize(
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        new_value: &str,
    ) -> Result<(), DebugError> {
        match variable.byte_size {
            Some(1) => i8::update_value(variable, memory, new_value),
            Some(2) => i16::update_value(variable, memory, new_value),
            Some(8) => i64::update_value(variable, memory, new_value),
            Some(16) => i128::update_value(variable, memory, new_value),
            _ => i32::update_value(variable, memory, new_value),
        }
    }
}

impl ProgrammingLanguage for Rust {
    fn read_variable_value(
        &self,
        variable: &Variable,
        memory: &mut dyn MemoryInterface,
        variable_cache: &VariableCache,
    ) -> VariableValue {
        match variable.type_name.inner() {
            VariableType::Base(_) if variable.memory_location == VariableLocation::Unknown => {
                VariableValue::Empty
            }

            VariableType::Base(type_name) => match type_name.as_str() {
                "!" => VariableValue::Valid("<Never returns>".to_string()),
                "()" => VariableValue::Valid("()".to_string()),
                "bool" => bool::get_value(variable, memory, variable_cache).map_or_else(
                    |err| VariableValue::Error(err.to_string()),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "char" => char::get_value(variable, memory, variable_cache).into(),
                "i8" => i8::get_value(variable, memory, variable_cache).into(),
                "i16" => i16::get_value(variable, memory, variable_cache).into(),
                "i32" => i32::get_value(variable, memory, variable_cache).into(),
                "i64" => i64::get_value(variable, memory, variable_cache).into(),
                "i128" => i128::get_value(variable, memory, variable_cache).into(),
                "isize" => Self::read_isize(variable, memory, variable_cache),
                "u8" => u8::get_value(variable, memory, variable_cache).into(),
                "u16" => u16::get_value(variable, memory, variable_cache).into(),
                "u32" => u32::get_value(variable, memory, variable_cache).into(),
                "u64" => u64::get_value(variable, memory, variable_cache).into(),
                "u128" => u128::get_value(variable, memory, variable_cache).into(),
                "usize" => Self::read_usize(variable, memory, variable_cache),
                "f32" => f32::get_value(variable, memory, variable_cache)
                    .map(|f| format_float(f as f64))
                    .into(),
                "f64" => f64::get_value(variable, memory, variable_cache)
                    .map(format_float)
                    .into(),
                "None" => VariableValue::Valid("None".to_string()),

                _undetermined_value => VariableValue::Empty,
            },
            VariableType::Struct(name) if name.ident_stem() == "&str" => {
                String::get_value(variable, memory, variable_cache).into()
            }
            VariableType::Other(name) if name == "!" => {
                VariableValue::Valid("<Never returns>".to_string())
            }
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
                "bool" => bool::update_value(variable, memory, new_value),
                "char" => char::update_value(variable, memory, new_value),
                "i8" => i8::update_value(variable, memory, new_value),
                "i16" => i16::update_value(variable, memory, new_value),
                "i32" => i32::update_value(variable, memory, new_value),
                "i64" => i64::update_value(variable, memory, new_value),
                "i128" => i128::update_value(variable, memory, new_value),
                "isize" => Self::update_isize(variable, memory, new_value),
                "u8" => u8::update_value(variable, memory, new_value),
                "u16" => u16::update_value(variable, memory, new_value),
                "u32" => u32::update_value(variable, memory, new_value),
                "u64" => u64::update_value(variable, memory, new_value),
                "u128" => u128::update_value(variable, memory, new_value),
                "usize" => Self::update_usize(variable, memory, new_value),
                "f32" => f32::update_value(variable, memory, new_value),
                "f64" => f64::update_value(variable, memory, new_value),
                other => Err(DebugError::WarnAndContinue {
                    message: format!("Updating {other} variables is not yet supported."),
                }),
            },
            other => Err(DebugError::WarnAndContinue {
                message: format!("Updating {} variables is not yet supported.", other.kind()),
            }),
        }
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
        let ptr_type = pointee.unwrap_or("<unknown pointer>");

        if ptr_type.starts_with(['*', '&']) {
            ptr_type.to_string()
        } else {
            // FIXME: we should track where the type name came from - the pointer node, or the pointee.
            format!("*raw {ptr_type}")
        }
    }

    fn parse_type_name(&self, name: &str) -> Option<VariableType> {
        type_name::parse_variable_type(name)
    }

    fn format_named_head(&self, ident: &str, args: &[String]) -> Option<String> {
        type_name::format_named_head(ident, args)
    }

    fn is_path_ident(&self, ident: &str) -> bool {
        type_name::is_path_ident(ident)
    }

    fn compact_debug_name(&self, name: &str) -> String {
        type_name::compact_debug_name(name)
    }

    fn format_function_name(
        &self,
        function_name: &str,
        function_die: &crate::function_die::FunctionDie<'_>,
        debug_info: &super::DebugInfo,
    ) -> String {
        if let Some(name) = synthesise_generated_fn(function_name, function_die, debug_info) {
            return name;
        }

        let function_name = self.compact_debug_name(function_name);
        let parent = function_die.parent_offset();
        if let Some((parent_unit, parent_offset)) = parent
            && let Ok(die) = parent_unit.unit.entry(parent_offset)
            && is_datatype(&die)
            && let Ok(Some(typename)) = parent_unit.extract_type_name(debug_info, &die)
        {
            let typename = self.compact_debug_name(&typename);
            if let Some((_, type_generic)) = typename.split_once('<')
                && let Some((function_without_generic, function_generic)) =
                    function_name.split_once('<')
                && type_generic == function_generic
            {
                format!("{typename}::{function_without_generic}")
            } else {
                format!("{typename}::{function_name}")
            }
        } else if let Some(name) = qualified_method_name(function_die, debug_info, &function_name) {
            name
        } else {
            function_name
        }
    }

    fn auto_resolve_children(&self, ty: &VariableType) -> bool {
        // Do not match `Some`, `Ok`, or `Err`. `process_variant` already expands the
        // active variant. Matching `Err` as a prefix also matched `Error`.
        let Some(ident) = ty.ident() else {
            return false;
        };
        ident.starts_with("&str")
            || ident.starts_with("&[")
            || ident == "Option"
            || ident == "Result"
    }

    fn is_side_effecting(&self, ty: &VariableType) -> bool {
        ty.named()
            .and_then(RustPath::from_named)
            .is_some_and(|path| path.is_volatile_cell())
    }

    fn process_struct(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: &StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        simplify_types::process_struct(
            unit_info, debug_info, node, variable, memory, cache, frame_info,
        )
    }
}

fn is_datatype(entry: &Die) -> bool {
    matches!(
        entry.tag(),
        gimli::DW_TAG_structure_type
            | gimli::DW_TAG_class_type
            | gimli::DW_TAG_union_type
            | gimli::DW_TAG_enumeration_type
    )
}

fn synthesise_generated_fn(
    function_name: &str,
    function_die: &crate::function_die::FunctionDie<'_>,
    debug_info: &DebugInfo,
) -> Option<String> {
    let ident = type_ident(function_name);
    let mut segments = declaration_enclosing_path(function_die, debug_info);
    segments.push(ident.to_string());

    if let Some(name) = type_name::synthesise_async_name(&segments) {
        return Some(name);
    }
    if type_name::is_closure_ident(ident) {
        let from_die = type_name::synthesise_closure_name(&segments);
        let from_link = demangled_linkage_name(function_die, debug_info).and_then(|name| {
            let compact = type_name::compact_debug_name(&name);
            (compact.contains("::") || compact.starts_with('<')).then_some(compact)
        });
        return match (from_die, from_link) {
            (Some(die), Some(link)) if link.contains("{impl") && !die.contains("{impl") => {
                Some(die)
            }
            (Some(die), Some(link)) if die.contains("{impl") && !link.contains("{impl") => {
                Some(link)
            }
            (die, link) => link.or(die),
        };
    }
    None
}

fn qualified_method_name(
    function_die: &crate::function_die::FunctionDie<'_>,
    debug_info: &DebugInfo,
    function_name: &str,
) -> Option<String> {
    if function_name.contains("::") || function_name.starts_with('<') {
        return None;
    }
    let demangled = demangled_linkage_name(function_die, debug_info)?;
    let label = type_name::associated_method_label(&demangled)?;
    Some(type_name::apply_dwarf_generics(&label, function_name))
}

fn declaration_enclosing_path(
    function_die: &crate::function_die::FunctionDie<'_>,
    debug_info: &DebugInfo,
) -> Vec<String> {
    let (unit, die) = function_die
        .abstract_die
        .as_ref()
        .or(function_die.specification_die.as_ref())
        .map(|(unit, die)| (*unit, die))
        .unwrap_or((function_die.unit_info, &function_die.function_die));
    unit.enclosing_path(debug_info, die)
}

fn demangled_linkage_name(
    function_die: &crate::function_die::FunctionDie<'_>,
    debug_info: &DebugInfo,
) -> Option<String> {
    let (unit, attr) = function_die.attribute_with_unit(debug_info, gimli::DW_AT_linkage_name)?;
    let raw = debug_info
        .dwarf
        .attr_string(&unit.unit, attr.value())
        .ok()?;
    let mangled = String::from_utf8_lossy(&raw);
    addr2line::demangle(mangled.as_ref(), gimli::DW_LANG_Rust)
}

/// Last path segment of a rust type name, without generic arguments.
fn type_ident(name: &str) -> &str {
    let before_generics = name.split_once('<').map_or(name, |(head, _)| head);
    before_generics
        .rsplit_once("::")
        .map_or(before_generics, |(_, ident)| ident)
}

/// Crate, modules, and ident from DWARF namespaces plus `DW_AT_name`.
#[derive(Clone)]
struct RustPath {
    crate_name: String,
    modules: Vec<String>,
    ident: String,
}

impl RustPath {
    fn from_die(
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        entry: &DebuggingInformationEntry<GimliReader>,
    ) -> Option<Self> {
        let name = unit_info.extract_type_name(debug_info, entry).ok()??;
        let mut namespaces = unit_info.namespace_path(debug_info, entry);
        if namespaces.is_empty() {
            return None;
        }
        let ident = type_ident(&name).to_string();
        let crate_name = namespaces.remove(0);
        Some(Self {
            crate_name,
            modules: namespaces,
            ident,
        })
    }

    fn from_variable(debug_info: &DebugInfo, variable: &Variable) -> Option<Self> {
        if let Some(named) = variable.type_name.named() {
            return Self::from_named(named);
        }
        let offset = variable.type_node_offset?;
        let (unit_info, entry) = debug_info.entry_at_debug_info_offset(offset).ok()?;
        Self::from_die(unit_info, debug_info, &entry)
    }

    fn from_named(named: &crate::NamedType) -> Option<Self> {
        let mut namespaces = named.namespace.to_vec();
        if namespaces.is_empty() {
            return None;
        }
        let crate_name = namespaces.remove(0);
        Some(Self {
            crate_name,
            modules: namespaces,
            ident: named.ident_stem().to_string(),
        })
    }

    fn is_rustc_lib(&self) -> bool {
        simplify_types::STDLIB
            .split('|')
            .any(|k| self.crate_name == k)
    }

    fn modules_start_with(&self, prefix: &[&str]) -> bool {
        self.modules.len() >= prefix.len()
            && self
                .modules
                .iter()
                .zip(prefix)
                .all(|(segment, expected)| segment == expected)
    }

    fn modules_eq(&self, modules: &[&str]) -> bool {
        self.modules.len() == modules.len()
            && self
                .modules
                .iter()
                .zip(modules)
                .all(|(segment, expected)| segment == expected)
    }

    /// `VolatileCell` holds a device register. A read of the register can clear a flag, or move a
    /// FIFO on, so the debugger must not read it on its own.
    fn is_volatile_cell(&self) -> bool {
        self.crate_name == "vcell" && self.ident == "VolatileCell"
    }

    fn is_maybe_uninit(&self) -> bool {
        self.is_rustc_lib() && self.ident == "MaybeUninit" && self.modules_start_with(&["mem"])
    }

    /// A wrapper that heapless (and the standard library) uses only to hold a value in storage.
    fn is_storage_wrapper(&self) -> bool {
        self.is_maybe_uninit()
            || (self.is_rustc_lib()
                && self.modules_start_with(&["mem"])
                && matches!(self.ident.as_str(), "ManuallyDrop" | "MaybeDangling"))
    }

    fn is_heapless_vec(&self) -> bool {
        self.crate_name == "heapless" && matches!(self.ident.as_str(), "Vec" | "VecInner")
    }
}

fn is_named(variable: &Variable, name: &str) -> bool {
    matches!(variable.name, VariableName::Named(ref var_name) if var_name == name)
}
