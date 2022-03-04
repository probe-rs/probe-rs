use super::*;
use crate::Error;
use anyhow::anyhow;
use gimli::{DebugInfoOffset, UnitOffset};
use num_traits::Zero;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
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
    /// Creates a new [`VariableCache`].
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
            log::trace!(
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

    /// Check if a `Variable` has any children. This also validates that the parent exists in the cache, before attempting to check for children.
    pub fn has_children(&self, parent_variable: &Variable) -> Result<bool, Error> {
        self.get_children(Some(parent_variable.variable_key))
            .map(|children| !children.is_empty())
    }

    /// Sometimes DWARF uses intermediate nodes that are not part of the coded variable structure.
    /// When we encounter them, the children of such intermediate nodes are assigned to the parent of the intermediate node, and we discard the intermediate nodes from the `DebugInfo::VariableCache`
    ///
    /// Similarly, while resolving [VariableNodeType::is_deferred()], i.e. 'lazy load' of variables, we need to create intermediate variables that are eliminated here.
    ///
    /// NOTE: For all other situations, this function will silently do nothing.
    pub fn adopt_grand_children(
        &mut self,
        parent_variable: &Variable,
        obsolete_child_variable: &Variable,
    ) -> Result<(), Error> {
        if obsolete_child_variable.type_name.is_empty()
            || obsolete_child_variable.variable_node_type != VariableNodeType::DoNotRecurse
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

/// Define the role that a variable plays in a Variant relationship. See section '5.7.10 Variant Entries' of the DWARF 5 specification
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum VariantRole {
    /// A (parent) Variable that can have any number of Variant's as its value
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

/// A [Variable] will have either a valid value, or some reason why a value could not be constructed.
/// - If we encounter expected errors, they will be displayed to the user as defined below.
/// - If we encounter unexpected errors, they will be treated as proper errors and will propogated to the calling process as an `Err()`
#[derive(Clone, Debug, PartialEq)]
pub enum VariableValue {
    /// A valid value of this variable
    Valid(String),
    /// Notify the user that we encountered a problem correctly resolving the variable.
    /// - The variable will be visible to the user, as will the other field of the variable.
    /// - The contained warning message will be displayed to the user.
    /// - The debugger will not attempt to resolve additional fields or children of this variable.
    Error(String),
    /// The value has not been set. This could be because ...
    /// - It is too early in the process to have discovered its value, or ...
    /// - The variable cannot have a stored value, e.g. a `struct`. In this case, please use `Variable::get_value` to infer a human readable value from the value of the struct's fields.
    Empty,
}

impl Default for VariableValue {
    fn default() -> Self {
        VariableValue::Empty
    }
}

impl std::fmt::Display for VariableValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VariableValue::Valid(value) => value.fmt(f),
            VariableValue::Error(error) => write!(f, "< {} >", error,),
            VariableValue::Empty => write!(
                f,
                "Value not set. Please use Variable::get_value() to infer a human readable variable value"
            ),
        }
    }
}

impl VariableValue {
    /// A VariableValue is valid if it doesn't contain an Info or a Warning.
    pub fn is_valid(&self) -> bool {
        !matches!(self, VariableValue::Error(_))
    }
    /// No value or error is present
    pub fn is_empty(&self) -> bool {
        matches!(self, VariableValue::Empty)
    }
}

/// The type of variable we have at hand.
#[derive(Debug, PartialEq, Clone)]
pub enum VariableName {
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
    /// A Namespace with a specific name
    Namespace(String),
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
            VariableName::StaticScope => write!(f, "<static_scope>"),
            VariableName::Registers => write!(f, "<registers>"),
            VariableName::LocalScope => write!(f, "<local_scope>"),
            VariableName::Artifical => write!(f, "<artifical>"),
            VariableName::AnonymousNamespace => write!(f, "<anonymous_namespace>"),
            VariableName::Namespace(name) => name.fmt(f),
            VariableName::Named(name) => name.fmt(f),
            VariableName::Unknown => write!(f, "<unknown>"),
        }
    }
}

/// Encode the nature of the Debug Information Entry in a way that we can resolve child nodes of a [Variable]
/// The rules for 'lazy loading'/deferred recursion of [Variable] children are described under each of the enum values.
#[derive(Debug, PartialEq, Clone)]
pub enum VariableNodeType {
    /// For pointer values, their referenced variables are found at an [gimli::UnitOffset] in the [DebugInfo].
    /// - Rule: Pointers to `struct` variables WILL NOT BE recursed, because  this may lead to infinite loops/stack overflows in `struct`s that self-reference.
    /// - Rule: Pointers to "base" datatypes SHOULD BE, but ARE NOT resolved, because it would keep the UX simple, but DWARF doesn't make it easy to determine when a pointer points to a base data type. We can read ahead in the DIE children, but that feels rather inefficient.
    ReferenceOffset(UnitOffset),
    /// Use the `header_offset` and `type_offset` as direct references for recursing the variable children. With the current implementation, the `type_offset` will point to a DIE with a tag of `DW_TAG_structure_type`.
    /// - Rule: For structured variables, we WILL NOT automatically expand their children, but we have enough information to expand it on demand. Except if they fall into one of the special cases handled by [VariableNodeType::RecurseAsIntermediate]
    TypeOffset(UnitOffset),
    /// Use the `header_offset` and `entries_offset` as direct references for recursing the variable children.
    /// - Rule: All top level variables in a [StackFrame] are automatically deferred, i.e [VariableName::StaticScope], [VariableName::Registers], [VariableName::LocalScope].
    DirectLookup,
    /// Sometimes it doesn't make sense to recurse the children of a specific node type
    /// - Rule: Pointers to `unit` datatypes WILL NOT BE resolved, because it doesn't make sense.
    /// - Rule: Once we determine that a variable can not be recursed further, we update the variable_node_type to indicate that no further recursion is possible/required. This can be because the variable is a 'base' data type, or because there was some kind of error in processing the current node, so we don't want to incur cascading errors.
    /// TODO: Find code instances where we use magic values (e.g. u32::MAX) and replace with DoNotRecurse logic if appropriate.
    DoNotRecurse,
    /// Unless otherwise specified, always recurse the children of every node until we get to the base data type.
    /// - Rule: (Default) Unless it is prevented by any of the other rules, we always recurse the children of these variables.
    /// - Rule: Certain structured variables (e.g. `&str`, `Some`, `Ok`, `Err`, etc.) are set to [VariableNodeType::RecurseToBaseType] to improve the debugger UX.
    /// - Rule: Pointers to `const` variables WILL ALWAYS BE recursed, because they provide essential information, for example about the length of strings, or the size of arrays.
    /// - Rule: Enumerated types WILL ALWAYS BE recursed, because we only ever want to see the 'active' child as the value.
    /// - Rule: For now, Array types WILL ALWAYS BE recursed. TODO: Evaluate if it is beneficial to defer these.
    /// - Rule: For now, Union types WILL ALWAYS BE recursed. TODO: Evaluate if it is beneficial to defer these.
    RecurseToBaseType,
}

impl VariableNodeType {
    pub fn is_deferred(&self) -> bool {
        match self {
            VariableNodeType::ReferenceOffset(_)
            | VariableNodeType::TypeOffset(_)
            | VariableNodeType::DirectLookup => true,
            _other => false,
        }
    }
}

impl Default for VariableNodeType {
    fn default() -> Self {
        VariableNodeType::RecurseToBaseType
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
    /// Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    value: VariableValue,
    /// The source location of the declaration of this variable, if available.
    pub source_location: Option<SourceLocation>,
    /// The name of the type of this variable.
    pub type_name: String,
    /// The unit_header_offset and variable_unit_offset are cached to allow on-demand access to the variable's gimli::Unit, through functions like:
    ///   `gimli::Read::DebugInfo.header_from_offset()`, and   
    ///   `gimli::Read::UnitHeader.entries_tree()`
    pub unit_header_offset: Option<DebugInfoOffset>,
    /// The offset of this variable into the compilation unit debug information.
    pub variable_unit_offset: Option<UnitOffset>,
    /// For 'lazy loading' of certain variable types we have to determine if the variable recursion should be deferred, and if so, how to resolve it when the request for further recursion happens.
    /// See [VariableNodeType] for more information.
    pub variable_node_type: VariableNodeType,
    /// The starting location/address in memory where this Variable's value is stored.
    pub memory_location: u64,
    /// The size of this variable in bytes.
    pub byte_size: u64,
    /// If  this is a subrange (array, vector, etc.), is the ordinal position of this variable in that range
    pub member_index: Option<i64>,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the lower bound.
    pub range_lower_bound: i64,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the the upper bound of the range.
    pub range_upper_bound: i64,
    /// The role of this variable.
    pub role: VariantRole,
}

impl Variable {
    /// In most cases, Variables will be initialized with their ELF references so that we resolve their data types and values on demand.
    pub(crate) fn new(
        header_offset: Option<DebugInfoOffset>,
        entries_offset: Option<UnitOffset>,
    ) -> Variable {
        Variable {
            unit_header_offset: header_offset,
            variable_unit_offset: entries_offset,
            ..Default::default()
        }
    }

    /// Implementing set_value(), because the library passes errors into the value of the variable.
    /// This ensures debug front ends can see the errors, but doesn't fail because of a single variable not being able to decode correctly.
    pub(crate) fn set_value(&mut self, new_value: VariableValue) {
        // Allow some block when logic requires it.
        #[allow(clippy::if_same_then_else)]
        if new_value.is_valid() {
            // Simply overwrite existing value with a new valid one.
            self.value = new_value;
        } else if self.value.is_valid() {
            // Overwrite a valid value with an error.
            self.value = new_value;
        } else {
            // Concatenate the error messages ...
            self.value = VariableValue::Error(format!("{} : {}", self.value, new_value));
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self, variable_cache: &VariableCache) -> String {
        // Allow for chained `if let` without complaining
        #[allow(clippy::if_same_then_else)]
        if !self.value.is_empty() {
            // The `value` for this `Variable` is non empty because ...
            // - It is base data type for which a value was determined based on the core runtime, or ...
            // - We encountered an error somewhere, so report it to the user
            format!("{}", self.value)
        } else if let VariableName::AnonymousNamespace = self.name {
            // Namespaces do not have values
            String::new()
        } else if let VariableName::Namespace(_) = self.name {
            // Namespaces do not have values
            String::new()
        } else {
            // We need to construct a 'human readable' value using `fmt::Display` to represent the values of complex types and pointers.
            match variable_cache.has_children(self) {
                Ok(has_children) => {
                    if has_children {
                        self.formatted_variable_value(variable_cache, 0_usize)
                    } else if self.type_name.is_empty() || self.memory_location.is_zero() {
                        if self.variable_node_type.is_deferred() {
                            // When we will do a lazy-load of variable children, and they have not yet been requested by the user, just display the type_name as the value
                            self.type_name.clone()
                        } else {
                            // This condition should only be true for intermediate nodes from DWARF. These should not show up in the final `VariableCache`
                            // If a user sees this error, then there is a logic problem in the stack unwind
                            "Error: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
                        }
                    } else {
                        format!(
                            "Unimplemented: Evaluate type {} of ({} bytes) at location 0x{:08x}",
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
        if !self.value.is_empty()
        // The value was set explicitly, so just leave it as is., or it was an error, so don't attempt anything else
        || self.memory_location == u64::MAX
        // Early on in the process of `Variable` evaluation
        || self.type_name.is_empty()
        // Templates, Phantoms, etc.
        || self.memory_location.is_zero()
        {
            return;
        } else if self.variable_node_type.is_deferred() {
            // And we have not previously assigned the value, then assign the type and address as the value
            self.value = VariableValue::Valid(format!(
                "{} @ {:#010X}",
                self.type_name.clone(),
                self.memory_location
            ));
            return;
        }

        log::trace!(
            "Extracting value for {:?}, type={}",
            self.name,
            self.type_name
        );

        // This is the primary logic for decoding a variable's value, once we know the type and memory_location.
        let known_value = match self.type_name.as_str() {
            "!" => VariableValue::Valid("<Never returns>".to_string()),
            "()" => VariableValue::Valid("()".to_string()),
            "bool" => bool::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "char" => char::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "&str" => String::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                VariableValue::Valid,
            ),
            "i8" => i8::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "i16" => i16::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "i32" => i32::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "i64" => i64::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "i128" => i128::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "isize" => isize::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "u8" => u8::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "u16" => u16::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "u32" => u32::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "u64" => u64::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "u128" => u128::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "usize" => usize::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "f32" => f32::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "f64" => f64::get_value(self, core, variable_cache).map_or_else(
                |err| VariableValue::Error(format!("{:?}", err)),
                |value| VariableValue::Valid(value.to_string()),
            ),
            "None" => VariableValue::Valid("None".to_string()),
            _undetermined_value => VariableValue::Empty,
        };
        self.value = known_value;
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

    /// `true` if the Variable has a valid value, or an empty value.
    /// `false` if the Variable has a VariableValue::Error(_)value
    pub fn is_valid(&self) -> bool {
        self.value.is_valid()
    }

    fn formatted_variable_value(
        &self,
        variable_cache: &VariableCache,
        indentation: usize,
    ) -> String {
        let line_feed = if indentation.is_zero() { "" } else { "\n" }.to_string();
        // Allow for chained `if let` without complaining
        #[allow(clippy::if_same_then_else)]
        if !self.value.is_empty() {
            // Use the supplied value or error message.
            format!(
                "{}{:\t<indentation$}{}: {}",
                line_feed, "", self.type_name, self.value
            )
        } else if let VariableName::AnonymousNamespace = self.name {
            // Namespaces do not have values
            String::new()
        } else if let VariableName::Namespace(_) = self.name {
            // Namespaces do not have values
            String::new()
        } else {
            // Infer a human readable value using the available children of this variable.
            let mut compound_value = "".to_string();
            if let Ok(children) = variable_cache.get_children(Some(self.variable_key)) {
                // Make sure we can safely unwrap() children.
                if self.type_name.starts_with('&') {
                    // Pointers
                    compound_value = format!(
                        "{}{}{:\t<indentation$}{}",
                        compound_value,
                        line_feed,
                        "",
                        if let Some(first_child) = children.first() {
                            first_child.formatted_variable_value(variable_cache, indentation + 1)
                        } else {
                            "Unable to resolve referenced variable value".to_string()
                        }
                    );
                    compound_value
                } else if self.type_name.starts_with('[') {
                    // Arrays
                    compound_value =
                        format!("{}{}{:\t<indentation$}[", compound_value, line_feed, "");
                    let mut child_count: usize = 0;
                    for child in children.iter() {
                        child_count += 1;
                        if child_count == children.len() {
                            // Do not add a separator at the end of the list
                            compound_value = format!(
                                "{}{}",
                                compound_value,
                                child.formatted_variable_value(variable_cache, indentation + 1)
                            );
                        } else {
                            compound_value = format!(
                                "{}{}, ",
                                compound_value,
                                child.formatted_variable_value(variable_cache, indentation + 1)
                            );
                        }
                    }
                    format!("{}{}{:\t<indentation$}]", compound_value, line_feed, "")
                } else if self.type_name.starts_with("Option")
                    || self.type_name.starts_with("Result")
                {
                    // For special structure types `Option<>` and `Result<>`, we only format their children
                    for child in children {
                        compound_value = format!(
                            "{}{}",
                            compound_value,
                            child.formatted_variable_value(variable_cache, indentation)
                        );
                    }
                    compound_value
                } else if self.type_name.as_str() == "Some"
                    || self.type_name.as_str() == "Ok"
                    || self.type_name.as_str() == "Err"
                {
                    // Handle special structure types like the variant values of `Option<>` and `Result<>`
                    compound_value = format!(
                        "{}{:\t<indentation$}{} {}(",
                        line_feed, "", self.type_name, compound_value
                    );
                    for child in children {
                        compound_value = format!(
                            "{}{}",
                            compound_value,
                            child.formatted_variable_value(variable_cache, indentation + 1)
                        );
                    }
                    format!("{}{}{:\t<indentation$})", compound_value, line_feed, "")
                } else {
                    // Generic handling of other structured types.
                    // The pre- and post- fix is determined by the type of children.
                    // compound_value = format!("{} {}", compound_value, self.type_name);
                    let (mut pre_fix, mut post_fix): (Option<String>, Option<String>) =
                        (None, None);
                    let mut child_count: usize = 0;
                    for child in children.iter() {
                        child_count += 1;
                        if pre_fix.is_none() && post_fix.is_none() {
                            if let VariableName::Named(child_name) = child.name.clone() {
                                if child_name.starts_with("__0") {
                                    // Treat this structure as a tuple
                                    pre_fix = Some(format!("{}{:\t<indentation$}(", line_feed, ""));
                                    post_fix =
                                        Some(format!("{}{:\t<indentation$})", line_feed, ""));
                                } else {
                                    // Treat this structure as a `struct`
                                    pre_fix = Some(format!(
                                        "{}{:\t<indentation$}{} {{",
                                        line_feed, "", self.type_name,
                                    ));
                                    post_fix =
                                        Some(format!("{}{:\t<indentation$}}}", line_feed, ""));
                                }
                            };
                            if let Some(pre_fix) = &pre_fix {
                                compound_value = format!("{}{}", compound_value, pre_fix);
                            };
                        }
                        if child_count == children.len() {
                            // Do not add a separator at the end of the list
                            compound_value = format!(
                                "{}{}",
                                compound_value,
                                child.formatted_variable_value(variable_cache, indentation + 1)
                            );
                        } else {
                            compound_value = format!(
                                "{}{}, ",
                                compound_value,
                                child.formatted_variable_value(variable_cache, indentation + 1)
                            );
                        }
                    }
                    if let Some(post_fix) = &post_fix {
                        compound_value = format!("{}{}", compound_value, post_fix);
                    };
                    compound_value
                }
            } else {
                // We don't have a value, and we can't generate one from children values, so use the type_name
                format!("{:\t<indentation$}{}", "", self.type_name)
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
                    Some(string_length) => {
                        if let VariableValue::Valid(length_value) = &string_length.value {
                            length_value.parse().unwrap_or(0_usize)
                        } else {
                            0_usize
                        }
                    }
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
                    str_value = "Error: Failed to determine &str memory location".to_string();
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
                str_value = "Error: Failed to evaluate &str value".to_string();
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
