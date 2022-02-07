use super::*;
use crate::Error;
use anyhow::anyhow;
use gimli::{DebugInfoOffset, UnitOffset};
use num_traits::Zero;

/// VariableCache stores every available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
/// NOTE: `VariableCache` must be refreshed (recreated) evertytime a stack trace is done.
///
/// We use 'special' `Variables`, to create a structure that models the tree strucutre used by MS DAP specification ( `Threads -> StackTrace -> Scopes -> Variables`), and later on allows the debugger to request these as parent-children structrues.)
///
/// In probe-rs, this `Variable` structure looks as follows (indentation represent parent-child structure)
/// - [VariableName::CoreId] : Every [`Core::id()`] is represented as a top level thread. e.g. Core 0 is thread 0, and so on. This is used as if it is synonomous to the `Threads` level in the MS DAP specification.
///   - [VariableName::StackFrame]: Every `StackFrame` in a stack trace will have one of these, with its function name captured in the `value` field of this variable.
///     - [VariableName::StaticScope]: The parent variable for all static scoped variables that are in the same compile unit, or in dependencies that are explicitly mentioned in the compile unit of the relevant stack frame.
///       - A recursive `Variable` structure as described for `[VariableName::LocalScope]` below.
///     - [VariableName::Registers]: Every `StackFrame` will have a collection of `Variable` registers with values scoped to that frame.
///       - A `Variable` for each available register
///     - [VariableName::LocalScope]: Every `StackFrame` (function) will have a collection of locally scoped `Variable`s.
///       - A `Variable` for each in-scope variable. Complex variables and pointers will have additional children.
///         - Child `Variable`s that make up a complex parent variable.
///           - This structure is recursive until a base type is encountered.
#[derive(Debug)]
pub struct VariableCache {
    variable_hash_map: HashMap<i64, Variable>,
}
impl Default for VariableCache {
    fn default() -> Self {
        Self::new()
    }
}

impl VariableCache {
    pub fn new() -> Self {
        VariableCache {
            variable_hash_map: HashMap::new(),
        }
    }

    /// Performs an *add* or *update* of a `probe_rs::debug::Variable` to the cache, consuming the input and returning a Clone.
    /// - *Add* operation: If the `Variable::variable_key` is 0, then assign a key and store it in the cache.
    ///   - Return an updated Clone of the stored variable
    /// - *Update* operation: If the `Variable::variable_key` is > 0
    ///   - If the key value exists in the cache, update it, Return an updated Clone of the variable.
    ///   - If the key value doesn't exist in the cache, Return an error.
    /// - For all operations, update the `parent_key`. A value of None means there are no parents for this variable.
    ///   - Validate that the supplied `Variable::parent_key` is a valid entry in the cache.
    /// - If appropriate, the `Variable::value` is updated from the core memory, and can be used by the calling function.
    pub fn cache_variable(
        &mut self,
        parent_key: Option<i64>,
        cache_variable: Variable,
        core: &mut Core<'_>,
    ) -> Result<Variable, Error> {
        let mut variable_to_add = cache_variable.clone();

        // Validate that the parent_key exists ...
        if let Some(new_parent_key) = parent_key {
            if self.variable_hash_map.contains_key(&new_parent_key) {
                variable_to_add.parent_key = parent_key;
            } else {
                return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {}. Please report this as a bug", variable_to_add.name, new_parent_key).into());
            }
        }

        // Is this an *add* or *update* operation?
        let stored_key = if variable_to_add.variable_key == 0 {
            // The caller is telling us this is definitely a new `Variable`
            variable_to_add.variable_key = get_sequential_key();

            log::trace!(
                "VariableCache: Add Variable: key={}, parent={:?}, name={:?}",
                variable_to_add.variable_key,
                variable_to_add.parent_key,
                &variable_to_add.name
            );

            let new_entry_key = variable_to_add.variable_key;
            if let Some(old_variable) = self
                .variable_hash_map
                .insert(variable_to_add.variable_key, variable_to_add)
            {
                return Err(anyhow!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {}. Please report this as a bug.", cache_variable.name, old_variable.variable_key).into());
            }
            new_entry_key
        } else {
            // Attempt to update an existing `Variable` in the cache
            log::debug!(
                "VariableCache: Update Variable, key={}, name={:?}",
                variable_to_add.variable_key,
                &variable_to_add.name
            );

            let updated_entry_key = variable_to_add.variable_key;
            if let Some(prev_entry) = self
                .variable_hash_map
                .get_mut(&variable_to_add.variable_key)
            {
                if &variable_to_add != prev_entry {
                    log::trace!("Updated:  {:?}", variable_to_add);
                    log::trace!("Previous: {:?}", prev_entry);
                }

                *prev_entry = variable_to_add
            } else {
                return Err(anyhow!("Attempt to update and existing `Variable`:{:?} with a non-existent cache key: {}. Please report this as a bug.", cache_variable.name, variable_to_add.variable_key).into());
            }

            updated_entry_key
        };

        // As the final act, we need to update the variable with an appropriate value.
        // This requires distinct steps to ensure we don't get `borrow` conflicts on the variable cache.
        if let Some(mut stored_variable) = self.get_variable_by_key(stored_key) {
            stored_variable.extract_value(core, self);
            if self
                .variable_hash_map
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
        self.variable_hash_map.get(&variable_key).cloned()
    }

    /// Retrieve a clone of a specific `Variable`, using the `name` and `parent_key`.
    /// If there is more than one, it will be logged (log::error!), and only the last will be returned.
    pub fn get_variable_by_name_and_parent(
        &self,
        variable_name: &VariableName,
        parent_key: Option<i64>,
    ) -> Option<Variable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                &child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .cloned()
            .collect::<Vec<Variable>>();
        match child_variables.len() {
            0 => None,
            1 => child_variables.first().cloned(),
            child_count => {
                log::error!("Found {} variables with parent_key={:?} and name={}. Please report this as a bug.", child_count, parent_key, variable_name);
                child_variables.last().cloned()
            }
        }
    }

    /// Retrieve `clone`d version of all the children of a `Variable`.
    /// If `parent_key == None`, it will return all the top level variables (no parents) in this cache.
    pub fn get_children(&self, parent_key: Option<i64>) -> Result<Vec<Variable>, Error> {
        let mut children: Vec<Variable> = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_key)
            .cloned()
            .collect::<Vec<Variable>>();
        // We have to incur the overhead of sort(), or else the variables in the UI are not in the same order as they appear in the source code.
        children.sort_by_key(|var| var.variable_key);
        Ok(children)
    }

    // Check if a `Variable` has any children. This also validates that the parent exists in the cache, before attempting to check for children.
    pub fn has_children(&self, parent_variable: &Variable) -> Result<bool, Error> {
        self.get_children(Some(parent_variable.variable_key))
            .map(|children| !children.is_empty())
    }

    /// Sometimes DWARF uses intermediate nodes that are not part of the coded variable structure.
    /// When we encounter them, the children of such intermediate nodes are assigned to the parent of the intermediate node, and we discard the intermediate nodes from the `DebugInfo::VariableCache`
    pub fn adopt_grand_children(
        &mut self,
        parent_variable: &Variable,
        obsolete_child_variable: &Variable,
    ) -> Result<(), Error> {
        // If the `obsolete_child_variable` has a type other than `Some`, then silently do nothing.
        if obsolete_child_variable.type_name.is_empty()
            || obsolete_child_variable.type_name == "Some"
            || obsolete_child_variable.name == VariableName::Named("*<statics>".to_string())
        {
            // Make sure we pass children up, past any intermediate nodes.
            self.variable_hash_map
                .values_mut()
                .filter(|search_variable| {
                    search_variable.parent_key == Some(obsolete_child_variable.variable_key)
                })
                .for_each(|grand_child| {
                    grand_child.parent_key = Some(parent_variable.variable_key)
                });
            // Remove the intermediate variable from the cache
            self.remove_cache_entry(obsolete_child_variable.variable_key)?;
        }
        Ok(())
    }

    /// Removing an entry's children from the `VariableCache` will recursively remove all their children
    pub fn remove_cache_entry_children(&mut self, parent_variable_key: i64) -> Result<(), Error> {
        let children: Vec<Variable> = self
            .variable_hash_map
            .values()
            .filter(|search_variable| search_variable.parent_key == Some(parent_variable_key))
            .cloned()
            .collect();
        for child in children {
            if self.variable_hash_map.remove(&child.variable_key).is_none() {
                return Err(anyhow!("Failed to remove a `VariableCache` entry with key: {}. Please report this as a bug.", child.variable_key).into());
            };
        }
        Ok(())
    }
    /// Removing an entry from the `VariableCache` will recursively remove all its children
    pub fn remove_cache_entry(&mut self, variable_key: i64) -> Result<(), Error> {
        self.remove_cache_entry_children(variable_key)?;
        if self.variable_hash_map.remove(&variable_key).is_none() {
            return Err(anyhow!("Failed to remove a `VariableCache` entry with key: {}. Please report this as a bug.", variable_key).into());
        };
        Ok(())
    }
}

impl std::fmt::Display for VariableCache {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let mut stack_frames = self
            .variable_hash_map
            .values()
            .cloned()
            .filter(|child_variable| {
                child_variable.name == VariableName::StackFrame && child_variable.parent_key == None
            })
            .collect::<Vec<Variable>>();
        stack_frames.sort_by_key(|variable| variable.variable_key);
        if stack_frames.is_empty() {
            writeln!(
                f,
                "`DebugInfo::VariableCache` contains no `StackFrame` data."
            )?;
        }

        for stackframe_root_variable in stack_frames {
            // Header info for the StackFrame
            writeln!(f)?;
            writeln!(
                f,
                "StackFrame data for {}",
                stackframe_root_variable
                    .value
                    .unwrap_or_else(|| "unknown function".to_string())
            )?;
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
        }
        writeln!(f)
    }
}

// TODO: For the CLI, implement new commands to view the values of variables. Including it in the std::fmt::Display for VariableCache is too noisy.
fn _fmt_recurse_variables(
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
        parent_variable.name,
        parent_variable.value.as_ref().unwrap_or(&"".to_string()),
        parent_variable.type_name
    );
    if let Ok(children) = variable_cache.get_children(Some(parent_variable.variable_key)) {
        for variable in &children {
            _fmt_recurse_variables(variable_cache, variable, new_level, f)?;
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

#[derive(Debug, PartialEq, Clone)]
pub enum VariableName {
    /// Every [`Core`] will have it's own set of hierarchy of entries in the cache.
    CoreId,
    /// Top-level variable for a stack frame (usually a function or inlined subroutine).
    StackFrame,
    /// Top-level variable for static variables, child of a stack frame variable, and holds all the static scoped variables which are directly visible to the compile unit of the frame.
    StaticScope,
    /// Top-level variable for registers, child of a stack frame variable.
    Registers,
    /// Top-level variable for local scoped variables, child of a stack frame variable.
    LocalScope,
    /// Artificial variable, without a name (e.g. enum discriminant)
    Artifical,
    /// Anonymous namespace
    AnonymousNamespace,
    /// Variable with a specific name
    Named(String),
    /// Variable with an unknown name
    Unknown,
}

impl Default for VariableName {
    fn default() -> Self {
        VariableName::Unknown
    }
}

impl std::fmt::Display for VariableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableName::CoreId => write!(f, "<core_id>"),
            VariableName::StackFrame => write!(f, "<stack_frame>"),
            VariableName::StaticScope => write!(f, "<static_scope>"),
            VariableName::Registers => write!(f, "<registers>"),
            VariableName::LocalScope => write!(f, "<local_scope>"),
            VariableName::Artifical => write!(f, "<artifical>"),
            VariableName::AnonymousNamespace => write!(f, "<anonymous_namespace>"),
            VariableName::Named(name) => name.fmt(f),
            VariableName::Unknown => write!(f, "<unknown>"),
        }
    }
}

/// The `Variable` struct is used in conjunction with `VariableCache` to cache data about variables.
///
/// Any modifications to the `Variable` value will be transient (lost when it goes out of scope),
/// unless it is updated through one of the available methods on `VariableCache`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Variable {
    /// Every variable must have a unique key value assigned to it. The value will be zero until it is stored in VariableCache, at which time its value will be set to the same as the VariableCache::variable_cache_key
    pub variable_key: i64,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache. A parent_key of None in the cache simply implies that this variable doesn't have a parent, i.e. it is the root of a tree.
    pub parent_key: Option<i64>,

    /// The variable name refers to the name of any of the types of values described in the [VariableCache]
    pub name: VariableName,

    /// The value will always be `None` unless the variable is a base type or there was an error during the unwind operation for the variable value. For all Variables that are complex types or references, the value will be a "fmt::Display" representation that attempts to assemble the base types into human readable form. Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    value: Option<String>,

    /// If this is a variable for a stack frame, then this is the loation for the program counter in the stack frame.
    ///
    /// For other variables, this is the location of the declaration, if available.
    pub source_location: Option<SourceLocation>,
    pub type_name: String,
    /// When we encounter DW_TAG_pointer_type during ELF parsing, we store the `gimli::UnitOffset to the 'referened' node.
    /// This will later be used to determine if we need to **automatically** recurse the children of pointer types, or to "lazily" wait until a user explicitly requests the children of such a pointer type.
    /// By default, the automatic recursion follows these rules:
    /// - Pointers to `struct` `Variable`s WILL NOT BE recursed, because  this may lead to infinite loops/stack overflows in `struct`s that self-reference.
    /// - Pointers to `const` `Variable`s WILL BE recursed, because they provide essential information, for example about the length of strings, or the size of arrays.
    /// - Pointers to "base" datatypes SHOULD BE resolved, because it would keep the UX simple, but DWARF doesn't make it easy to determine when a pointer points to a base data type. We can read ahead in the DIE children, but that feels rather inefficient.
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
        if let Some(existing_value) = self.value.clone() {
            if new_value != existing_value {
                // We append the new value to the old value, so that we don't loose any prior errors or warnings originating from the process of decoding the actual value.
                self.value = Some(format!("{} : {}", existing_value, new_value));
            }
        } else {
            self.value = Some(new_value);
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self, variable_cache: &VariableCache) -> String {
        if let Some(existing_value) = self.value.clone() {
            // The `value` for this `Variable` is non empty because it is either:
            // - A base data type for which a value was determined based on the core runtime
            // - Contains an error that was encountered while resolving the `Variable`'s `DebugInfo`
            existing_value
        } else {
            // We need to construct a 'human readable' value using `fmt::Display` to represent the values of complex types and pointers.
            match variable_cache.has_children(self) {
                Ok(has_children) => {
                    if has_children {
                        self.formatted_variable_value(variable_cache)
                    } else if self.type_name.is_empty() || self.memory_location.is_zero() {
                        if self.referenced_node_offset.is_some() {
                            // When we will do a lazy-load of variable children, and they have not yet been requested by the user
                            "".to_string()
                        } else {
                            // This condition should only be true for intermediate nodes from DWARF. These should not show up in the final `VariableCache`
                            // If a user sees this error, then there is a logic problem in the stack unwind
                            "ERROR: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
                        }
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
        }
    }

    /// Evaluate the variable's result if possible and set self.value, or else set self.value as the error String.
    fn extract_value(&mut self, core: &mut Core<'_>, variable_cache: &VariableCache) {
        // Quick exit if we don't really need to do much more.
        // The value was set by get_location(), so just leave it as is.
        if self.memory_location == u64::MAX
        // The value was set elsewhere in this library - probably because of an error - so just leave it as is.
        || self.value.is_some()
        // Early on in the process of `Variable` evaluation
        || self.type_name.is_empty()
        // Templates, Phantoms, etc.
        || self.memory_location.is_zero()
        {
            return;
        } else if self.referenced_node_offset.is_some() {
            // And we have not previously assigned the value, then assign the type and address as the value
            self.value = Some(format!(
                "{} @ {:#010X}",
                self.type_name.clone(),
                self.memory_location
            ));
            return;
        }

        log::debug!(
            "Extracting value for {:?}, type={}",
            self.name,
            self.type_name
        );

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
        self.value = Some(string_value);
    }

    /// The variable is considered to be an 'indexed' variable if the name starts with two underscores followed by a number. e.g. "__1".
    /// TODO: Consider replacing this logic with `std::str::pattern::Pattern` when that API stabilizes
    pub fn is_indexed(&self) -> bool {
        match &self.name {
            VariableName::Named(name) => {
                name.starts_with("__")
                    && name
                        .find(char::is_numeric)
                        .map_or(false, |zero_based_position| zero_based_position == 2)
            }
            // Other kind of variables are never indexed
            _ => false,
        }
    }

    fn formatted_variable_value(&self, variable_cache: &VariableCache) -> String {
        if let Some(existing_value) = &self.value {
            // Use the supplied value.
            existing_value.clone()
        } else {
            let mut compound_value = "".to_string();
            // Only do this if we do not already have a value assigned.
            if let Ok(children) = variable_cache.get_children(Some(self.variable_key)) {
                // Make sure we can safely unwrap() children.
                if self.type_name.starts_with('&') {
                    // Pointers
                    compound_value = format!(
                        "{}{}",
                        compound_value,
                        if let Some(first_child) = children.first() {
                            first_child.formatted_variable_value(variable_cache)
                        } else {
                            "Unable to resolve referenced variable value".to_string()
                        }
                    );
                    compound_value
                } else if self.type_name.starts_with('(') {
                    // Tuples
                    compound_value = format!("{}(", compound_value);
                    for child in children {
                        compound_value = format!(
                            "{}{}, ",
                            compound_value,
                            child.formatted_variable_value(variable_cache)
                        );
                    }
                    format!("{})", compound_value)
                } else if self.type_name.starts_with('[') {
                    // Arrays
                    compound_value = format!("{}[", compound_value);
                    for child in children {
                        compound_value = format!(
                            "{}{}, ",
                            compound_value,
                            child.formatted_variable_value(variable_cache)
                        );
                    }
                    format!("{}]", compound_value)
                } else {
                    // Generic handling of other structured types.
                    // TODO: This is 'ok' for most, but could benefit from some custom formatting, e.g. Unions, Result<> and Option<>
                    if self.is_indexed() {
                        // Indexed variables look different ...
                        compound_value = format!("{}{}:{{", compound_value, self.name);
                    } else {
                        compound_value = format!("{}{{", compound_value);
                    }
                    for child in children {
                        compound_value = format!(
                            "{}{}, ",
                            compound_value,
                            child.formatted_variable_value(variable_cache)
                        );
                    }
                    format!("{}}}", compound_value)
                }
            } else {
                // We don't have a value, and we can't generate one from children values, so use the type_name
                self.type_name.to_string()
            }
        }
    }
}

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
        if let Some(return_value) = char::from_u32(mem_data) {
            Ok(return_value)
        } else {
            Ok('?')
        }
    }
}

impl Value for String {
    fn get_value(
        variable: &Variable,
        core: &mut Core<'_>,
        variable_cache: &VariableCache,
    ) -> Result<Self, DebugError> {
        let mut str_value: String = "".to_owned();
        if let Ok(children) = variable_cache.get_children(Some(variable.variable_key)) {
            if !children.is_empty() {
                let mut string_length = match children.iter().find(|child_variable| {
                    child_variable.name == VariableName::Named("length".to_string())
                }) {
                    Some(length_value) => length_value
                        .value
                        .as_ref()
                        .map(|value| value.parse().unwrap_or(0))
                        .unwrap_or(0) as usize,
                    None => 0_usize,
                };
                let string_location = match children.iter().find(|child_variable| {
                    child_variable.name == VariableName::Named("data_ptr".to_string())
                }) {
                    Some(location_value) => {
                        if let Ok(child_variables) =
                            variable_cache.get_children(Some(location_value.variable_key))
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
                    // Limit string length to work around buggy information, otherwise the debugger
                    // can hang due to buggy debug information.
                    //
                    // TODO: If implemented, the variable should not be fetched automatically,
                    // but only when requested by the user. This workaround can then be removed.
                    if string_length > 200 {
                        log::warn!(
                            "Very long string ({} bytes), truncating to 200 bytes.",
                            string_length
                        );
                        string_length = 200;
                    }

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
        // TODO: We can get the actual WORD length from [DWARF] instead of assuming `u32`
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
        // TODO: We can get the actual WORD length from [DWARF] instead of assuming `u32`
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
