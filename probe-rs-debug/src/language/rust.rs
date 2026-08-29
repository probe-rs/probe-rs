use crate::{
    DebugError, DebugInfo, GimliReader, ObjectRef, Variable, VariableCache, VariableLocation,
    VariableName, VariableNodeType, VariableType, VariableValue,
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

    /// Unwraps some common newtypes after their member DIE is in the cache.
    #[expect(clippy::too_many_arguments)]
    fn flatten_known_wrapper(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(path) = RustPath::from_die(unit_info, debug_info, node) else {
            return Ok(());
        };
        if !path.is_transparent_wrapper() {
            return Ok(());
        }

        let children: Vec<_> = cache.get_children(variable.variable_key).cloned().collect();
        let [inner] = children.as_slice() else {
            return Ok(());
        };

        let mut inner = inner.clone();
        debug_info.cache_deferred_variables(cache, memory, &mut inner, frame_info)?;
        let Some(inner) = cache.get_variable_by_key(inner.variable_key) else {
            return Ok(());
        };

        let inner_is_unsafe_cell = RustPath::from_variable(debug_info, &inner)
            .is_some_and(|path| path.is_core_unsafe_cell());

        if inner_is_unsafe_cell {
            cache.adopt_grand_children(variable, &inner)?;
            return Ok(());
        }

        if !cache.has_children(&inner) && inner.value.is_valid() && !inner.value.is_empty() {
            if path.ident == "ManuallyDrop" {
                variable.type_name = inner.type_name;
            }
            variable.set_value(inner.value.clone());
            cache.remove_cache_entry(inner.variable_key)?;
            variable.variable_node_type = VariableNodeType::DoNotRecurse;
            cache.update_variable(variable)?;
        }

        Ok(())
    }

    /// Shows the initialized prefix of a `heapless::Vec` buffer as a slice.
    #[expect(clippy::too_many_arguments)]
    fn expand_heapless_vec(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(path) = RustPath::from_die(unit_info, debug_info, node) else {
            return Ok(());
        };
        if !path.is_heapless_vec() {
            return Ok(());
        }

        let children: Vec<_> = cache.get_children(variable.variable_key).cloned().collect();
        let Some(len) = children
            .iter()
            .find(|c| is_named(c, "len"))
            .and_then(|field| match &field.value {
                VariableValue::Valid(len) => len.parse::<u64>().ok(),
                _ => None,
            })
        else {
            return Ok(());
        };
        let Some(mut buffer) = children.iter().find(|c| is_named(c, "buffer")).cloned() else {
            return Ok(());
        };

        debug_info.cache_deferred_variables(cache, memory, &mut buffer, frame_info)?;
        let Some(buffer) = cache.get_variable_by_key(buffer.variable_key) else {
            return Ok(());
        };
        let Some(buffer) = Self::storage_array(debug_info, memory, cache, frame_info, buffer)?
        else {
            return Ok(());
        };

        let elements: Vec<_> = cache
            .get_children(buffer.variable_key)
            .filter(|c| matches!(c.name, VariableName::Indexed(_)))
            .cloned()
            .collect();

        for element in &elements {
            let VariableName::Indexed(index) = element.name else {
                continue;
            };
            if index >= len {
                cache.remove_cache_entry(element.variable_key)?;
            }
        }

        let live: Vec<_> = cache
            .get_children(buffer.variable_key)
            .filter(|c| matches!(c.name, VariableName::Indexed(index) if index < len))
            .cloned()
            .collect();

        for mut element in live {
            self.unwrap_maybe_uninit_slot(debug_info, memory, cache, frame_info, &mut element)?;
        }

        let Some(mut buffer) = cache.get_variable_by_key(buffer.variable_key) else {
            return Ok(());
        };

        if let Some(first) = cache.get_children(buffer.variable_key).next().cloned() {
            buffer.type_name = VariableType::Array {
                item_type_name: Box::new(first.type_name.clone()),
                count: len as usize,
            };
            if let Some(item_size) = first.byte_size {
                buffer.byte_size = Some(item_size * len);
            }
        } else if let VariableType::Array { count, .. } = &mut buffer.type_name {
            *count = 0;
            buffer.byte_size = Some(0);
        }
        buffer.value = VariableValue::Empty;
        cache.update_variable(&buffer)?;

        Ok(())
    }

    fn storage_array(
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
        buffer: Variable,
    ) -> Result<Option<Variable>, DebugError> {
        if matches!(buffer.type_name, VariableType::Array { .. }) {
            return Ok(Some(buffer));
        }

        let inner: Vec<_> = cache.get_children(buffer.variable_key).cloned().collect();
        let [inner] = inner.as_slice() else {
            return Ok(None);
        };
        let mut inner = inner.clone();
        debug_info.cache_deferred_variables(cache, memory, &mut inner, frame_info)?;
        let Some(inner) = cache.get_variable_by_key(inner.variable_key) else {
            return Ok(None);
        };
        if matches!(inner.type_name, VariableType::Array { .. }) {
            Ok(Some(inner))
        } else {
            Ok(None)
        }
    }

    fn unwrap_maybe_uninit_slot(
        &self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
        element: &mut Variable,
    ) -> Result<(), DebugError> {
        debug_info.cache_deferred_variables(cache, memory, element, frame_info)?;
        let Some(element_now) = cache.get_variable_by_key(element.variable_key) else {
            return Ok(());
        };
        let Some(path) = RustPath::from_variable(debug_info, &element_now) else {
            return Ok(());
        };
        if !path.is_maybe_uninit() {
            return Ok(());
        }

        let children: Vec<_> = cache
            .get_children(element_now.variable_key)
            .cloned()
            .collect();
        let Some(mut value) = children.iter().find(|c| is_named(c, "value")).cloned() else {
            return Ok(());
        };

        debug_info.cache_deferred_variables(cache, memory, &mut value, frame_info)?;
        let Some(value) = cache.get_variable_by_key(value.variable_key) else {
            return Ok(());
        };

        let mut element = element_now;
        element.type_name = value.type_name.clone();
        element.byte_size = value.byte_size;

        if cache.has_children(&value) {
            for child in &children {
                if child.variable_key != value.variable_key {
                    cache.remove_cache_entry(child.variable_key)?;
                }
            }
            cache.adopt_grand_children(&element, &value)?;
            element.value = VariableValue::Empty;
        } else {
            element.set_value(value.value.clone());
            cache.remove_cache_entry_children(element.variable_key)?;
            element.variable_node_type = VariableNodeType::DoNotRecurse;
        }

        cache.update_variable(&element)?;
        Ok(())
    }

    /// Replaces *const data pointer with *const [data; len] in slices.
    ///
    /// This function may return `Ok(())` even if it does not modify the variable.
    fn expand_slice(
        &self,
        debug_info: &DebugInfo,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        let Some(slice) = Self::try_get_slice(variable, cache) else {
            return Ok(());
        };

        // Turn the data pointer into an array.
        let pointer_key = slice.data_ptr.variable_key;
        let length = slice.length;

        let Some(mut pointee) = cache.get_children(pointer_key).next().cloned() else {
            return Ok(());
        };

        // Do we know the type of the data?
        let Some(type_node_offset) = pointee.type_node_offset else {
            return Ok(());
        };

        // Let's just remove the pointer. While it may be interesting where the data is, the
        // address can be read using the debugger, and is otherwise just noise on the UI.
        cache.remove_cache_entry(pointer_key)?;

        // Replace the pointee type with an array of known length. We don't have to modify the
        // memory location, as the pointer is already pointing to the first element of the array.
        pointee.parent_key = ObjectRef::Invalid;
        pointee.variable_key = ObjectRef::Invalid;
        pointee.value = VariableValue::Empty;
        pointee.type_name = VariableType::Array {
            item_type_name: { Box::new(pointee.type_name) },
            count: length as usize,
        };
        pointee.variable_node_type = VariableNodeType::RecurseToBaseType;

        cache.add_variable(variable.variable_key, &mut pointee)?;

        let (member_unit, array_member_type_node) =
            debug_info.entry_at_debug_info_offset(type_node_offset)?;

        let member_range = 0..length;
        member_unit.expand_array_members(
            debug_info,
            &array_member_type_node,
            cache,
            &mut pointee,
            memory,
            &[member_range],
            frame_info,
        )?;

        Ok(())
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
            VariableType::Struct(name) if name == "&str" => {
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
        VariableValue::Valid(format!("{}::{}", type_name.display_name(self), value))
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

    fn format_function_name(
        &self,
        function_name: &str,
        function_die: &crate::function_die::FunctionDie<'_>,
        debug_info: &super::DebugInfo,
    ) -> String {
        let parent = function_die.parent_offset();
        if let Some((parent_unit, parent_offset)) = parent
            && let Ok(die) = parent_unit.unit.entry(parent_offset)
            && is_datatype(&die)
            && let Ok(Some(typename)) = parent_unit.extract_type_name(debug_info, &die)
        {
            // TODO: apply better heuristics to clean up the final function name
            if let Some((_, type_generic)) = typename.split_once('<')
                && let Some((function_without_generic, function_generic)) =
                    function_name.split_once('<')
                && type_generic == function_generic
            {
                format!("{typename}::{function_without_generic}")
            } else {
                format!("{typename}::{function_name}")
            }
        } else {
            function_name.to_string()
        }
    }

    fn auto_resolve_children(&self, name: &str) -> bool {
        // Do not match `Some`, `Ok`, or `Err`. `process_variant` already expands the
        // active variant. `starts_with("Err")` also matched `Error` and walked
        // those types on the first expand.
        name.starts_with("&str")
            || name.starts_with("&[")
            || is_rust_type(name, "Option")
            || is_rust_type(name, "Result")
    }

    fn process_struct(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        node: &DebuggingInformationEntry<GimliReader>,
        variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
        cache: &mut VariableCache,
        frame_info: StackFrameInfo<'_>,
    ) -> Result<(), DebugError> {
        if variable.type_name().starts_with("&[") {
            self.expand_slice(debug_info, variable, memory, cache, frame_info)?;
        }

        self.flatten_known_wrapper(
            unit_info, debug_info, node, variable, memory, cache, frame_info,
        )?;
        self.expand_heapless_vec(
            unit_info, debug_info, node, variable, memory, cache, frame_info,
        )?;

        Ok(())
    }
}

fn is_datatype(entry: &Die) -> bool {
    [gimli::DW_TAG_structure_type, gimli::DW_TAG_enumeration_type].contains(&entry.tag())
}

/// Last path segment of a rust type name, without generic arguments.
fn type_ident(name: &str) -> &str {
    let before_generics = name.split_once('<').map_or(name, |(head, _)| head);
    before_generics
        .rsplit_once("::")
        .map_or(before_generics, |(_, ident)| ident)
}

/// Crate, modules, and ident from DWARF namespaces plus `DW_AT_name`.
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
        let offset = variable.type_node_offset?;
        let (unit_info, entry) = debug_info.entry_at_debug_info_offset(offset).ok()?;
        Self::from_die(unit_info, debug_info, &entry)
    }

    fn is_rustc_lib(&self) -> bool {
        matches!(self.crate_name.as_str(), "core" | "alloc" | "std")
    }

    fn modules_start_with(&self, prefix: &[&str]) -> bool {
        self.modules.len() >= prefix.len()
            && self
                .modules
                .iter()
                .zip(prefix)
                .all(|(segment, expected)| segment == expected)
    }

    fn is_transparent_wrapper(&self) -> bool {
        if self.crate_name == "embassy_executor"
            && self.modules_start_with(&["raw", "util"])
            && self.ident == "SyncUnsafeCell"
        {
            return true;
        }
        if self.crate_name == "vcell" && self.modules.is_empty() && self.ident == "VolatileCell" {
            return true;
        }

        if !self.is_rustc_lib() {
            return false;
        }

        match self.ident.as_str() {
            "Cell" | "SyncUnsafeCell" => self.modules_start_with(&["cell"]),
            "ManuallyDrop" => self.modules_start_with(&["mem"]),
            "Wrapping" | "NonZero" => self.modules_start_with(&["num"]),
            ident if ident.starts_with("Atomic") => self.modules_start_with(&["sync", "atomic"]),
            _ => false,
        }
    }

    fn is_core_unsafe_cell(&self) -> bool {
        self.is_rustc_lib() && self.ident == "UnsafeCell" && self.modules_start_with(&["cell"])
    }

    fn is_maybe_uninit(&self) -> bool {
        self.is_rustc_lib() && self.ident == "MaybeUninit" && self.modules_start_with(&["mem"])
    }

    fn is_heapless_vec(&self) -> bool {
        self.crate_name == "heapless" && self.ident == "Vec"
    }
}

fn is_named(variable: &Variable, name: &str) -> bool {
    matches!(variable.name, VariableName::Named(ref var_name) if var_name == name)
}

/// `type_name`, `prefix::type_name`, or those names with a generic argument list.
fn is_rust_type(name: &str, type_name: &str) -> bool {
    type_ident(name) == type_name
}

#[cfg(test)]
mod tests {
    use super::{RustPath, is_rust_type};
    use crate::DebugInfo;

    #[test]
    fn rust_type_name_matches() {
        assert!(is_rust_type("Option", "Option"));
        assert!(is_rust_type(
            "Option<&mut probe_rs_debugger_test::RecursiveStruct>",
            "Option"
        ));
        assert!(is_rust_type("core::option::Option<u32>", "Option"));
        assert!(!is_rust_type("Error", "Err"));
        assert!(!is_rust_type("Optional<u32>", "Option"));
        assert!(is_rust_type("Result<T, E>", "Result"));
        assert!(is_rust_type("UnsafeCell<u32>", "UnsafeCell"));
    }

    #[test]
    fn rustc_and_embassy_wrappers_match_their_namespaces() {
        assert!(path("core", &["cell"], "Cell").is_transparent_wrapper());
        assert!(!path("core", &["cell"], "UnsafeCell").is_transparent_wrapper());
        assert!(path("core", &["cell"], "UnsafeCell").is_core_unsafe_cell());
        assert!(path("core", &["sync", "atomic"], "AtomicU32").is_transparent_wrapper());
        assert!(path("core", &["mem", "manually_drop"], "ManuallyDrop").is_transparent_wrapper());
        assert!(path("core", &["num", "wrapping"], "Wrapping").is_transparent_wrapper());
        assert!(path("core", &["num", "nonzero"], "NonZero").is_transparent_wrapper());
        assert!(
            path("embassy_executor", &["raw", "util"], "SyncUnsafeCell").is_transparent_wrapper()
        );

        assert!(!path("portable_atomic", &[], "AtomicU32").is_transparent_wrapper());
        assert!(
            !path(
                "embassy_sync",
                &["waitqueue", "atomic_waker"],
                "AtomicWaker"
            )
            .is_transparent_wrapper()
        );
        assert!(!path("esp_hal", &["sync", "multicore"], "AtomicLock").is_transparent_wrapper());
        assert!(!path("core", &["cell"], "RefCell").is_transparent_wrapper());
        assert!(!path("core", &["pin"], "Pin").is_transparent_wrapper());
        assert!(!path("core", &["ptr", "non_null"], "NonNull").is_transparent_wrapper());
        assert!(path("heapless", &["vec"], "Vec").is_heapless_vec());
        assert!(!path("alloc", &["vec"], "Vec").is_heapless_vec());
        assert!(path("vcell", &[], "VolatileCell").is_transparent_wrapper());
    }

    #[test]
    fn dwarf_namespaces_identify_the_crate() {
        let elf = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/debug-unwind-tests/esp32s3_esp_hal_panic.elf");
        let debug_info = DebugInfo::from_file(&elf).unwrap();

        let mut saw_core_cell = false;
        let mut saw_core_atomic = false;
        let mut saw_portable_atomic = false;
        for unit in &debug_info.unit_infos {
            let mut cursor = unit.unit.entries();
            while let Ok(Some(entry)) = cursor.next_dfs() {
                if entry.tag() != gimli::DW_TAG_structure_type {
                    continue;
                }
                let Some(path) = RustPath::from_die(unit, &debug_info, entry) else {
                    continue;
                };
                if path.ident == "Cell" && path.crate_name == "core" {
                    saw_core_cell = true;
                    assert_eq!(path.modules, ["cell"]);
                    assert!(path.is_transparent_wrapper());
                }
                if path.ident == "AtomicU32" && path.crate_name == "core" {
                    saw_core_atomic = true;
                    assert_eq!(path.modules, ["sync", "atomic"]);
                    assert!(path.is_transparent_wrapper());
                }
                if path.ident == "AtomicU32" && path.crate_name == "portable_atomic" {
                    saw_portable_atomic = true;
                    assert!(!path.is_transparent_wrapper());
                }
            }
        }
        assert!(saw_core_cell);
        assert!(saw_core_atomic);
        assert!(saw_portable_atomic);
    }

    fn path(crate_name: &str, modules: &[&str], ident: &str) -> RustPath {
        RustPath {
            crate_name: crate_name.to_string(),
            modules: modules.iter().map(|s| (*s).to_string()).collect(),
            ident: ident.to_string(),
        }
    }
}
