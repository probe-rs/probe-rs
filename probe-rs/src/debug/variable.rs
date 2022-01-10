use super::*;
use crate::Error;
use anyhow::anyhow;
use gimli::{DebugInfoOffset, UnitOffset};
use num_traits::Zero;
use std::{
    cell::{Cell, RefCell},
    cmp::Ordering,
    convert::TryInto,
    fmt,
};

/// VariableCache stores every available `Variable`, and provides methods to create and navigate the parent-child relationships of the Variables.
/// There should be ONLY ONE `VariableCache` per `DebugInfo`. Because of the multiple ways in which it is updated, all references to `VariableCache` are *immutable*, and can only be updated through its methods, which provide *interior mutability"
///
/// There are four 'dummy' `Variables`, named `<statics>`, `<stack_frame>`, `<registers>`, and `<locals>`. These are used to provide the header structure of how variables relate to different scopes in a particular stacktrace. This 'dummy' structure looks as follows
/// - `<statics>`: The parent variable for all static coped variables in the stack
///   - A recursive `Variable` structure as described for `<locals>` below.
/// - `<stack_frame>`: Every `StackFrame` will have one of these, with its function name captured in the `value` field of this dummy variable.
///   - `<registers>`: Every `StackFrame` will have a collection of `Variable` registers.
///     - A `Variable` for each available register
///   - `<locals>`: Every `StackFrame` (function) will have a collection of locally scoped `Variable`s.
///     - A `Variable` for each in-scope variable. Complex variables and pointers will have additional children.
///       - Child `Variable`s that make up a complex parent variable.
///         - This structure is recursive until a base type is encountered.
pub struct VariableCache {
    variable_cache_key: Cell<i64>,
    variable_hash_map: RefCell<HashMap<i64, Variable>>,
}
impl Default for VariableCache {
    fn default() -> Self {
        Self::new()
    }
}

impl VariableCache {
    pub fn new() -> Self {
        Self {
            variable_cache_key: Cell::new(0),
            variable_hash_map: RefCell::new(HashMap::new()),
        }
    }

    /// Performs an *add* or *update* of a `probe_rs::debug::Variable` to the cache, consuming the input and returning a Clone.
    /// - *Add* operation: If the `Variable::variable_key` is 0, then assign a key and store it in the cache.
    ///   - Return an updated Clone of the stored variable
    /// - *Update* operation: If the `Variable::variable_key` is > 0
    ///   - If the key value exists in the cache, update it, Return an updated Clone of the variable.
    ///   - If the key value doesn't exist in the cache, Return an error.
    /// - For all operations, update the `parent_key`. A value of 0 means there are no parents for this variable.
    ///   - Validate that the supplied `Variable::parent_key` is a valid entry in the cache.
    /// - If appropriate, the `Variable::value` is updated from the core memory, and can be used by the calling function.
    #[must_use]
    pub fn cache_variable(
        &self,
        parent_key: i64,
        cache_variable: Variable,
        core: &mut Core<'_>,
    ) -> Result<Variable, Error> {
        let mut variable_to_add = cache_variable.clone();

        // Validate that the parent_key exists ...
        variable_to_add.parent_key = parent_key;
        if variable_to_add.parent_key > 0
            && (!self
                .variable_hash_map
                .borrow()
                .contains_key(&variable_to_add.parent_key))
        {
            return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {}. Please report this as a bug", variable_to_add.name, variable_to_add.parent_key).into());
        }

        // Is this an *add* or *update* operation?
        let stored_key = if variable_to_add.variable_key == 0 {
            // The caller is telling us this is definitely a new `Variable`
            let new_cache_key: i64 = self.variable_cache_key.get() + 1;
            variable_to_add.variable_key = new_cache_key;
            match self
                .variable_hash_map
                .borrow_mut()
                .insert(variable_to_add.variable_key, variable_to_add)
            {
                Some(old_variable) => {
                    return Err(anyhow!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {}. Please report this as a bug.", cache_variable.name, old_variable.variable_key).into());
                }
                None => {
                    self.variable_cache_key.set(new_cache_key);
                }
            }
            self.variable_cache_key.get()
        } else {
            // Attempt to update an existing `Variable` in the cache
            let reused_cache_key = variable_to_add.variable_key;
            if self
                .variable_hash_map
                .borrow_mut()
                .insert(variable_to_add.variable_key, variable_to_add)
                .is_none()
            {
                return Err(anyhow!("Attempt to update and existing `Variable`:{:?} with a non-existent cache key: {}. Please report this as a bug.", cache_variable.name, reused_cache_key).into());
            }
            reused_cache_key
        };
        // As the final act, we need to update the variable with an appropriate value.
        // This requires distinct steps to ensure we don't get `borrow` conflicts on the variable cache.
        if let Some(mut stored_variable) = self.get_variable_by_key(stored_key) {
            stored_variable.extract_value(core, self);
            if self
                .variable_hash_map
                .borrow_mut()
                .insert(stored_variable.variable_key, stored_variable.clone())
                .is_none()
            {
                Err(anyhow!("Failed to store variable at variable_cache_key: {}. Please report this as a bug.", stored_key).into())
            } else {
                Ok(stored_variable)
            }
        } else {
            Err(anyhow!(
                "Failed to store variable at variable_cache_key: {}. Please report this as a bug.",
                stored_key
            )
            .into())
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `variable_key`.
    pub fn get_variable_by_key(&self, variable_key: i64) -> Option<Variable> {
        self.variable_hash_map.borrow().get(&variable_key).cloned()
    }

    /// Retrieve a clone of a specific `Variable`, using the `name` and `parent_key`.
    /// If there is more than one, it will be logged (log::warn!), and only the last will be returned.
    pub fn get_variable_by_name_and_parent(
        &self,
        variable_name: String,
        parent_key: i64,
    ) -> Option<Variable> {
        self.variable_hash_map
            .borrow()
            .values()
            .filter(|child_variable| {
                child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .last()
            .cloned()
    }

    /// Retrieve `clone`d version of all the children of a `Variable`.
    /// This also validates that the parent exists in the cache, before attempting to retrieve children.
    pub fn get_children(&self, parent_key: i64) -> Result<Vec<Variable>, Error> {
        if parent_key == 0 && (!self.variable_hash_map.borrow().contains_key(&parent_key)) {
            return Err(anyhow!("VariableCache: Attempted to retrieve children for a non existent `variable_key`: {}.", parent_key).into());
        } else {
            let mut children: Vec<Variable> = self
                .variable_hash_map
                .borrow()
                .values()
                .filter(|child_variable| child_variable.parent_key == parent_key)
                .cloned()
                .collect::<Vec<Variable>>();
            // We have to incur the overhead of sort(), or else the variables in the UI are not in the same order as they appear in the source code.
            children.sort();
            Ok(children)
        }
    }

    // Check if a `Variable` has any children. This also validates that the parent exists in the cache, before attempting to check for children.
    pub fn has_children(&self, parent_variable: &Variable) -> Result<bool, Error> {
        self.get_children(parent_variable.variable_key)
            .map(|children| !children.is_empty())
    }

    /// Sometimes DWARF uses intermediate nodes that are not part of the coded variable structure.
    /// When we encounter them, the children of such intermediate nodes are assigned to the parent of the intermediate node, and we discard the intermediate nodes from the `DebugInfo::VariableCache`
    pub fn adopt_grand_children(
        &self,
        parent_variable: &Variable,
        obsolete_child_variable: &Variable,
    ) -> Result<(), Error> {
        // If the `obsolete_child_variable` has a type, then silently do nothing.
        if obsolete_child_variable.type_name.is_empty() {
            // Make sure we pass children up, past any intermediate nodes.
            self.variable_hash_map
                .borrow_mut()
                .values_mut()
                .filter(|search_variable| {
                    search_variable.parent_key == obsolete_child_variable.variable_key
                })
                .for_each(|grand_child| grand_child.parent_key = parent_variable.variable_key);
            // Remove the intermediate variable from the cache
            self.remove_cache_entry(obsolete_child_variable.variable_key)?;
        }
        Ok(())
    }

    /// Removing an entry's children from the `VariableCache` will recursively remove all their children
    pub fn remove_cache_entry_children(&self, parent_variable_key: i64) -> Result<(), Error> {
        let children: Vec<Variable> = self
            .variable_hash_map
            .borrow()
            .values()
            .filter(|search_variable| search_variable.parent_key == parent_variable_key)
            .cloned()
            .collect();
        for child in children {
            if self
                .variable_hash_map
                .borrow_mut()
                .remove(&child.variable_key)
                .is_none()
            {
                return Err(anyhow!("Failed to remove a `VariableCache` entry with key: {}. Please report this as a bug.", child.variable_key).into());
            };
        }
        Ok(())
    }
    /// Removing an entry from the `VariableCache` will recursively remove all its children
    pub fn remove_cache_entry(&self, variable_key: i64) -> Result<(), Error> {
        self.remove_cache_entry_children(variable_key)?;
        if self
            .variable_hash_map
            .borrow_mut()
            .remove(&variable_key)
            .is_none()
        {
            return Err(anyhow!("Failed to remove a `VariableCache` entry with key: {}. Please report this as a bug.", variable_key).into());
        };
        Ok(())
    }
}

impl std::fmt::Display for VariableCache {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Some(static_root_variable) =
            self.get_variable_by_name_and_parent("<statics>".to_owned(), 0)
        {
            // Write the static variable data
            writeln!(
                f,
                "Static Variables for StackFrame {:?}",
                static_root_variable
            )?;
            fmt_recurse_variables(self, &static_root_variable, 0, f)?;
        } else {
            writeln!(
                f,
                "`DebugInfo::VariableCache` contains no data. Please report this as a bug."
            )?;
        }

        let mut stack_frames = self
            .variable_hash_map
            .borrow()
            .values()
            .cloned()
            .filter(|child_variable| {
                child_variable.name == "<stack_frame>".to_owned() && child_variable.parent_key == 0
            })
            .collect::<Vec<Variable>>();
        stack_frames.sort();
        if stack_frames.is_empty() {
            writeln!(
                f,
                "`DebugInfo::VariableCache` contains no `StackFrame` data."
            )?;
        }

        for stackframe_root_variable in stack_frames {
            // Header info for the StackFrame
            writeln!(f)?;
            writeln!(f, "StackFrame data for {}", stackframe_root_variable.value)?;
            if let Some(si) = stackframe_root_variable.source_location {
                write!(
                    f,
                    "\t{}/{}",
                    si.directory
                        .as_ref()
                        .map(|p| p.to_string_lossy())
                        .unwrap_or_else(|| std::borrow::Cow::from("<unknown dir>")),
                    si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
                )?;

                if let (Some(column), Some(line)) = (si.column, si.line) {
                    match column {
                        ColumnType::Column(c) => write!(f, ":{}:{}", line, c)?,
                        ColumnType::LeftEdge => write!(f, ":{}", line)?,
                    }
                }

                writeln!(f)?;
            }

            // Write the register variable data
            if let Some(register_root_variable) = self.get_variable_by_name_and_parent(
                "<registers>".to_owned(),
                stackframe_root_variable.variable_key,
            ) {
                writeln!(f)?;
                writeln!(
                    f,
                    "Registers for StackFrame {}",
                    stackframe_root_variable.value
                )?;
                fmt_recurse_variables(self, &register_root_variable, 0, f)?;
            } else {
                writeln!(
                    f,
                    "`DebugInfo::VariableCache` contains no `Register` data for `StackFrame` {}.",
                    stackframe_root_variable.value
                )?;
            }

            // Write the function variable data
            if let Some(function_root_variable) = self.get_variable_by_name_and_parent(
                "<locals>".to_owned(),
                stackframe_root_variable.variable_key,
            ) {
                writeln!(f)?;
                writeln!(
                    f,
                    "Function variables for StackFrame {}",
                    stackframe_root_variable.value
                )?;
                fmt_recurse_variables(self, &function_root_variable, 0, f)?;
            } else {
                writeln!(
                    f,
                    "`DebugInfo::VariableCache` contains no `Variable` data for `StackFrame` {}.",
                    stackframe_root_variable.value
                )?;
            }
        }

        writeln!(f)
    }
}

fn fmt_recurse_variables(
    variable_cache: &VariableCache,
    parent_variable: &Variable,
    level: u32,
    f: &mut std::fmt::Formatter,
) -> std::fmt::Result {
    for _depth in 0..level {
        write!(f, "   ")?;
    }
    let new_level = level + 1;
    let ret = writeln!(
        f,
        "|-> {} \t= {} \t({})",
        parent_variable.name, parent_variable.value, parent_variable.type_name
    ); // ... or if we want human readable values for complex variables, use `get_value())`;
    if let Ok(children) = variable_cache.get_children(parent_variable.variable_key) {
        for variable in &children {
            fmt_recurse_variables(variable_cache, variable, new_level, f)?;
        }
    }
    ret
}

/// Define the role that a variable plays in a Variant relationship. See section '5.7.10 Variant Entries' of the DWARF 5 specification
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VariantRole {
    /// A (parent) Variable that can have any number of Variant's as it's value
    VariantPart(u64),
    /// A (child) Variable that defines one of many possible types to hold the current value of a VariantPart.
    Variant(u64),
    /// This variable doesn't play a role in a Variant relationship
    NonVariant,
}

impl Default for VariantRole {
    fn default() -> Self {
        VariantRole::NonVariant
    }
}

/// The `Variable` struct is used in conjunction with `VariableCache` to cache data about variables.
///
/// Any modifications to the `Variable` value will be transient (lost when it goes out of scope),
/// unless it is updated through one of the available methods on `VariableCache`.
#[derive(Debug, Default, Clone)]
pub struct Variable {
    /// Every variable must have a unique key value assigned to it. The value will be zero until it is stored in VariableCache, at which time its value will be set to the same as the VariableCache::variable_cache_key
    pub variable_key: i64,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache A parent_key of 0 in the cache means it is the root of a variable tree.
    pub parent_key: i64,
    pub name: String,
    /// The value will always be an empty string unless the variable is a base type. For all Variables that are complex types or references, the value will be a "fmt::Display" representation that attempts to assemble the base types into human readable form. Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    pub(crate) value: String,
    pub source_location: Option<SourceLocation>,
    pub type_name: String,
    /// TODO: Update this documentation to reflect the code logic.
    /// When we encounter DW_TAG_pointer_type during ELF parsing, we store the `gimli::UnitOffset to the 'referened' node.
    /// This will later be used to determine if we need to **automatically** recurse the children of pointer types, or to wait until a user explicitly requests the children of such a pointer type.
    /// By default, the automatic recursion follows these rules:
    /// - Pointers to `struct` `Variable`s WILL NOT BE recursed, because  this may lead to infinite loops/stack overflows in `struct`s that self-reference.
    /// - Pointers to `const` `Variable`s WILL BE recursed, because they provide essential information, for example about the length of strings, or the size of arrays.
    /// - Pointers to "base" datatypes WILL BE resolved, because it keeps things simple.
    /// - Pointers to `unit` datatypes WILL NOT BE resolved, because it doesn't make sense.
    pub referenced_node_offset: Option<UnitOffset>,
    /// The header_offset and entries_offset are cached to allow on-demand access to the gimli::Unit, through functions like:
    ///   `gimli::Read::DebugInfo.header_from_offset()`, and   
    ///   `gimli::Read::UnitHeader.entries_tree()`
    ///
    /// TODO: Is there a more efficient method to get on demand access to gimli::Unit through stored references to it?
    pub header_offset: Option<DebugInfoOffset>,
    pub entries_offset: Option<UnitOffset>,
    /// The register values are needed to resolve the debug information and calculate memory locations and run-time data values. This is only needed for referenced nodes of variables with `DW_TAG_pointer_type`
    pub stack_frame_registers: Option<Registers>,
    /// The starting location/address in memory where this Variable's value is stored.
    pub memory_location: u64,
    pub byte_size: u64,
    /// If  this is a subrange (array, vector, etc.), is the ordinal position of this variable in that range
    pub member_index: Option<i64>,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the lower bound.
    pub range_lower_bound: i64,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the the upper bound of the range.
    pub range_upper_bound: i64,
    pub role: VariantRole,
}

impl PartialEq for Variable {
    fn eq(&self, other: &Self) -> bool {
        self.variable_key == other.variable_key
            || (self.parent_key == other.parent_key && self.name == other.name)
    }
}

impl Eq for Variable {}

impl Ord for Variable {
    fn cmp(&self, other: &Self) -> Ordering {
        self.variable_key.cmp(&other.variable_key)
    }
}

impl PartialOrd for Variable {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Variable {
    /// In most cases, Variables will be initialized with their ELF references, so that we resolve their data types and values on demand.
    pub(crate) fn new(
        header_offset: Option<DebugInfoOffset>,
        entries_offset: Option<UnitOffset>,
    ) -> Variable {
        Variable {
            header_offset,
            entries_offset,
            ..Default::default()
        }
    }

    /// Implementing set_value(), because the library passes errors into the value of the variable.
    /// This ensures debug front ends can see the errors, but doesn't fail because of a single variable not being able to decode correctly.
    pub(crate) fn set_value(&mut self, new_value: String) {
        if self.value.is_empty() {
            self.value = new_value;
        } else {
            // We append the new value to the old value, so that we don't loose any prior errors or warnings originating from the process of decoding the actual value.
            self.value = format!("{} : {}", self.value, new_value);
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self, variable_cache: &VariableCache) -> String {
        if self.value.is_empty() {
            // We need to construct a 'human readable' value using `fmt::Display` to represent the values of complex types and pointers.
            match variable_cache.has_children(self) {
                Ok(has_children) => {
                    if has_children {
                        //TODO: Recurse through all descendants ... change {:?} to {}
                        format!("{:?}", self)
                    } else if self.type_name.is_empty() || self.memory_location.is_zero() {
                        // Intermediate nodes from DWARF. These should not show up in the final `VariableCache`
                        "ERROR: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
                    } else {
                        format!(
                            "UNIMPLEMENTED: Evaluate type {} of ({} bytes) at location 0x{:08x}",
                            self.type_name, self.byte_size, self.memory_location
                        )
                    }
                }
                Err(error) => format!(
                    "Failed to determine children for `Variable`:{}. {:?}",
                    self.name, error
                ),
            }
        } else {
            // The `value` for this `Variable` is non empty because it is either:
            // - A base data type for which a value was determined based on the core runtime
            // - Contains an error that was encountered while resolving the `Variable`'s `DebugInfo`
            self.value.clone()
        }
    }

    /// Evaluate the variable's result if possible and set self.value, or else set self.value as the error String.
    fn extract_value(&mut self, core: &mut Core<'_>, variable_cache: &VariableCache) {
        // Quick exit if we don't really need to do much more.
        // The value was set by get_location(), so just leave it as is.
        if self.memory_location == u64::MAX
        // The value was set elsewhere in this library - probably because of an error - so just leave it as is.
        || !self.value.is_empty()
        // Early on in the process of `Variable` evaluation
        || self.type_name.is_empty()
        // Templates, Phantoms, etc.
        || self.memory_location.is_zero()
        {
            return;
        } else if self.referenced_node_offset.is_some() {
            // And we have not previously assigned the value, then assign the type and address as the value
            self.value = format!(
                "{} @ 0x{:08X}",
                self.type_name.clone(),
                self.memory_location
            );
            return;
        }

        // This is the primary logic for decoding a variable's value, once we know the type and memory_location.
        let string_value = match self.type_name.as_str() {
            "!" => "<Never returns>".to_string(),
            "()" => "()".to_string(),
            "bool" => bool::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "char" => char::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "&str" => String::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value),
            "i8" => i8::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "i16" => i16::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "i32" => i32::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "i64" => i64::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "i128" => i128::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "isize" => isize::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u8" => u8::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u16" => u16::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u32" => u32::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u64" => u64::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "u128" => u128::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "usize" => usize::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "f32" => f32::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "f64" => f64::get_value(self, core, variable_cache)
                .map_or_else(|err| format!("ERROR: {:?}", err), |value| value.to_string()),
            "None" => "None".to_string(),
            _undetermined_value => "".to_owned(),
        };
        self.value = string_value;
    }
}

// TODO: Fix this
// impl fmt::Display for Variable {
//     fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
//         if self.value.is_empty() {
//             // Only do this if we do not already have a value assigned.
//             if let Some(children) = self.children.clone() {
//                 // Make sure we can safely unwrap() children.
//                 if self.type_name.starts_with('&') {
//                     // Pointers
//                     write!(f, "{}", children.first().unwrap())
//                 } else if self.type_name.starts_with('(') {
//                     // Tuples
//                     write!(f, "(")?;
//                     for child in children {
//                         write!(f, "{}, ", child)?;
//                     }
//                     write!(f, ")")
//                 } else if self.type_name.starts_with('[') {
//                     // Arrays
//                     write!(f, "[")?;
//                     for child in children {
//                         write!(f, "{}, ", child)?;
//                     }
//                     write!(f, "]")
//                 } else {
//                     // Generic handling of other structured types.
//                     // TODO: This is 'ok' for most, but could benefit from some custom formatting, e.g. Unions.
//                     if self.name.starts_with("__") {
//                         // Indexed variables look different ...
//                         write!(f, "{}:{{", self.name)?;
//                     } else {
//                         write!(f, "{{")?;
//                     }
//                     for child in children {
//                         write!(f, "{}, ", child)?;
//                     }
//                     write!(f, "}}")
//                 }
//             } else {
//                 // Unknown.
//                 write!(f, "{}", self.type_name)
//             }
//         } else {
//             // Use the supplied value.
//             write!(f, "{}", self.value)
//         }
//     }
// }

/// Traits and Impl's to read from memory and decode the Variable value based on Variable::typ and Variable::location.
/// The MS DAP protocol passes the value as a string, so these are here only to provide the memory read logic before returning it as a string.
trait Value {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError>
    where
        Self: Sized;
}

impl Value for bool {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mem_data = core.read_word_8(variable.memory_location as u32)?;
        let ret_value: bool = mem_data != 0;
        Ok(ret_value)
    }
}
impl Value for char {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mem_data = core.read_word_32(variable.memory_location as u32)?;
        // TODO: Use char::from_u32 once it stabilizes.
        let ret_value: char = mem_data.try_into()?;
        Ok(ret_value)
    }
}

impl Value for String {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut str_value: String = "".to_owned();
        if let Ok(children) = variable_cache.get_children(variable.variable_key) {
            if !children.is_empty() {
                let string_length = match children
                    .clone()
                    .into_iter()
                    .find(|child_variable| child_variable.name == *"length")
                {
                    Some(length_value) => length_value.value.parse().unwrap_or(0) as usize,
                    None => 0_usize,
                };
                let string_location = match children
                    .into_iter()
                    .find(|child_variable| child_variable.name == *"data_ptr")
                {
                    Some(location_value) => {
                        if let Ok(child_variables) =
                            variable_cache.get_children(location_value.variable_key)
                        {
                            if let Some(first_child) = child_variables.first() {
                                first_child.memory_location as u32
                            } else {
                                0_u32
                            }
                        } else {
                            0_u32
                        }
                    }
                    None => 0_u32,
                };
                if string_location.is_zero() {
                    str_value = "ERROR: Failed to determine &str memory location".to_string();
                } else {
                    let mut buff = vec![0u8; string_length];
                    core.read(string_location as u32, &mut buff)?;
                    str_value = core::str::from_utf8(&buff)?.to_owned();
                }
            } else {
                str_value = "ERROR: Failed to evaluate &str value".to_string();
            }
        };
        Ok(str_value)
    }
}
impl Value for i8 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i8::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i16 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i16::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i32 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i64 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for i128 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = i128::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for isize {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        // TODO: how to get the MCU isize calculated for all platforms.
        let ret_value = i32::from_le_bytes(buff);
        Ok(ret_value as isize)
    }
}

impl Value for u8 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 1];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u8::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u16 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 2];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u16::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u32 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u64 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for u128 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 16];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = u128::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for usize {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        // TODO: how to get the MCU usize calculated for all platforms.
        let ret_value = u32::from_le_bytes(buff);
        Ok(ret_value as usize)
    }
}
impl Value for f32 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 4];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = f32::from_le_bytes(buff);
        Ok(ret_value)
    }
}
impl Value for f64 {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        _variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut buff = [0u8; 8];
        core.read(variable.memory_location as u32, &mut buff)?;
        let ret_value = f64::from_le_bytes(buff);
        Ok(ret_value)
    }
}
