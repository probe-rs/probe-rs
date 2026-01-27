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
            matches!(var.name, VariableName::Named(ref var_name) if var_name == name)
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

    /// Replaces *const data pointer with *const [data; len] in slices.
    ///
    /// This function may return `Ok(())` even if it does not modify the variable.
    #[expect(clippy::too_many_arguments)]
    fn expand_slice(
        &self,
        unit_info: &UnitInfo,
        debug_info: &DebugInfo,
        _node: &DebuggingInformationEntry<GimliReader>,
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

        let array_member_type_node = unit_info
            .unit
            .entry(type_node_offset)
            .expect("Failed to get array member type node. This is a bug, please report it!");

        let member_range = 0..length;
        unit_info.expand_array_members(
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
                    |err| VariableValue::Error(format!("{err:?}")),
                    |value| VariableValue::Valid(value.to_string()),
                ),
                "char" => char::get_value(variable, memory, variable_cache).into(),
                "i8" => i8::get_value(variable, memory, variable_cache).into(),
                "i16" => i16::get_value(variable, memory, variable_cache).into(),
                "i32" => i32::get_value(variable, memory, variable_cache).into(),
                "i64" => i64::get_value(variable, memory, variable_cache).into(),
                "i128" => i128::get_value(variable, memory, variable_cache).into(),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `i32`
                "isize" => i32::get_value(variable, memory, variable_cache).into(),
                "u8" => u8::get_value(variable, memory, variable_cache).into(),
                "u16" => u16::get_value(variable, memory, variable_cache).into(),
                "u32" => u32::get_value(variable, memory, variable_cache).into(),
                "u64" => u64::get_value(variable, memory, variable_cache).into(),
                "u128" => u128::get_value(variable, memory, variable_cache).into(),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `u32`
                "usize" => u32::get_value(variable, memory, variable_cache).into(),
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
                // TODO: We can get the actual WORD length from DWARF instead of assuming `i32`
                "isize" => i32::update_value(variable, memory, new_value),
                "u8" => u8::update_value(variable, memory, new_value),
                "u16" => u16::update_value(variable, memory, new_value),
                "u32" => u32::update_value(variable, memory, new_value),
                "u64" => u64::update_value(variable, memory, new_value),
                "u128" => u128::update_value(variable, memory, new_value),
                // TODO: We can get the actual WORD length from DWARF instead of assuming `u32`
                "usize" => u32::update_value(variable, memory, new_value),
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
        if let Some(parent_offset) = parent
            && let Ok(die) = function_die.unit_info.unit.entry(parent_offset)
            && is_datatype(&die)
            && let Ok(Some(typename)) = function_die.unit_info.extract_type_name(debug_info, &die)
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
        name.starts_with("&str")
            || name.starts_with("&[")
            || name.starts_with("Option")
            || name.starts_with("Some")
            || name.starts_with("Result")
            || name.starts_with("Ok")
            || name.starts_with("Err")
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
            self.expand_slice(
                unit_info, debug_info, node, variable, memory, cache, frame_info,
            )?;
        }

        Ok(())
    }
}

fn is_datatype(entry: &Die) -> bool {
    [gimli::DW_TAG_structure_type, gimli::DW_TAG_enumeration_type].contains(&entry.tag())
}
