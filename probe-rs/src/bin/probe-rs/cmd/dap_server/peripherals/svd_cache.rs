use std::collections::BTreeMap;
use std::ops::Range;

use probe_rs::debug::{get_object_reference, DebugError, ObjectRef};
use probe_rs::{Error, MemoryInterface};

use anyhow::anyhow;
use serde::Serialize;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug, Clone, PartialEq)]
pub struct SvdVariableCache {
    root_variable_key: ObjectRef,

    variable_hash_map: BTreeMap<ObjectRef, SvdVariable>,
}

impl SvdVariableCache {
    fn new(mut variable: SvdVariable) -> Self {
        let key = get_object_reference();

        variable.variable_key = key;

        SvdVariableCache {
            root_variable_key: key,
            variable_hash_map: BTreeMap::from([(key, variable)]),
        }
    }

    /// Create a new cache for SVD variables
    pub fn new_svd_cache() -> Self {
        let mut device_root_variable = SvdVariable::new(
            SvdVariableName::PeripheralScopeRoot,
            SvdVariableType::Other("<Unknown>".to_string()),
        );
        device_root_variable.variable_node_type = SvdVariableNodeType::DoNotRecurse;

        SvdVariableCache::new(device_root_variable)
    }

    pub fn has_children(&self, parent_variable: &SvdVariable) -> Result<bool, Error> {
        self.get_children(parent_variable.variable_key)
            .map(|children| !children.is_empty())
    }

    /// Retrieve `clone`d version of all the children of a `Variable`.
    /// If `parent_key == None`, it will return all the top level variables (no parents) in this cache.
    pub fn get_children(&self, parent_key: ObjectRef) -> Result<Vec<SvdVariable>, Error> {
        let children: Vec<SvdVariable> = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_key)
            .cloned()
            .collect::<Vec<SvdVariable>>();

        Ok(children)
    }

    /// Retrieve a clone of a specific `Variable`, using the `variable_key`.
    pub fn get_variable_by_key(&self, variable_key: ObjectRef) -> Option<SvdVariable> {
        self.variable_hash_map.get(&variable_key).cloned()
    }

    /// Get the root variable of the cache
    pub fn root_variable(&self) -> SvdVariable {
        self.variable_hash_map[&self.root_variable_key].clone()
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &SvdVariableName) -> Option<SvdVariable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name))
            .collect::<Vec<&SvdVariable>>();

        match &child_variables[..] {
            [] => None,
            [variable] => Some((*variable).clone()),
            [first, ..] => {
                tracing::warn!(
                    "Found {} variables with name={}. Please report this as a bug.",
                    child_variables.len(),
                    variable_name
                );
                Some((*first).clone())
            }
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `name` and `parent_key`.
    /// If there is more than one, it will be logged (tracing::error!), and only the last will be returned.
    pub fn get_variable_by_name_and_parent(
        &self,
        variable_name: &SvdVariableName,
        parent_key: ObjectRef,
    ) -> Option<SvdVariable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                &child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .collect::<Vec<&SvdVariable>>();

        match &child_variables[..] {
            [] => None,
            [variable] => Some((*variable).clone()),
            [.., last] => {
                tracing::error!("Found {} variables with parent_key={:?} and name={}. Please report this as a bug.", child_variables.len(), parent_key, variable_name);
                Some((*last).clone())
            }
        }
    }

    pub fn add_variable(
        &mut self,
        parent_key: ObjectRef,
        cache_variable: &mut SvdVariable,
    ) -> Result<(), Error> {
        // Validate that the parent_key exists ...
        if self.variable_hash_map.contains_key(&parent_key) {
            cache_variable.parent_key = parent_key;
        } else {
            return Err(anyhow!("SvdVariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", cache_variable.name, parent_key).into());
        }

        if cache_variable.variable_key != ObjectRef::Invalid {
            return Err(anyhow!("SvdVariableCache: Attempted to add a new variable: {} with already set key: {:?}. Please report this as a bug", cache_variable.name, cache_variable.variable_key).into());
        }

        // The caller is telling us this is definitely a new `Variable`
        cache_variable.variable_key = get_object_reference();

        tracing::trace!(
            "SvdVariableCache: Add Variable: key={:?}, parent={:?}, name={:?}",
            cache_variable.variable_key,
            cache_variable.parent_key,
            &cache_variable.name
        );

        if let Some(old_variable) = self
            .variable_hash_map
            .insert(cache_variable.variable_key, cache_variable.clone())
        {
            return Err(anyhow!("Attempt to insert a new `SvdVariable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", cache_variable.name, old_variable.variable_key).into());
        }

        Ok(())
    }

    pub fn update_variable(&mut self, cache_variable: &SvdVariable) -> Result<(), Error> {
        if cache_variable.variable_key == ObjectRef::Invalid {
            return Err(anyhow!("Attempt to update an existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key).into());
        }

        // Attempt to update an existing `Variable` in the cache
        tracing::trace!(
            "SvdVariableCache: Update SvdVariable, key={:?}, name={:?}",
            cache_variable.variable_key,
            &cache_variable.name
        );

        if let Some(prev_entry) = self.variable_hash_map.get_mut(&cache_variable.variable_key) {
            if cache_variable != prev_entry {
                tracing::trace!("Updated:  {:?}", cache_variable);
                tracing::trace!("Previous: {:?}", prev_entry);
                *prev_entry = cache_variable.clone();
            }
        } else {
            return Err(anyhow!("Attempt to update an existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key).into());
        }

        Ok(())
    }
}

/// The `Variable` struct is used in conjunction with `VariableCache` to cache data about variables.
///
/// Any modifications to the `Variable` value will be transient (lost when it goes out of scope),
/// unless it is updated through one of the available methods on `VariableCache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvdVariable {
    /// Every variable must have a unique key value assigned to it. The value will be zero until it is stored in VariableCache, at which time its value will be set to the same as the VariableCache::variable_cache_key
    pub(super) variable_key: ObjectRef,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache.
    pub parent_key: ObjectRef,
    /// The variable name refers to the name of any of the types of values described in the [VariableCache]
    pub name: SvdVariableName,
    /// Use `Variable::set_value()` and `Variable::get_value()` to correctly process this `value`
    value: SvdVariableValue,

    /// The name of the type of this variable.
    pub type_name: SvdVariableType,
    /// For 'lazy loading' of certain variable types we have to determine if the variable recursion should be deferred, and if so, how to resolve it when the request for further recursion happens.
    /// See [VariableNodeType] for more information.
    pub variable_node_type: SvdVariableNodeType,
    /// The starting location/address in memory where this Variable's value is stored.
    pub memory_location: SvdVariableLocation,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the lower bound.
    pub range_lower_bound: i64,
    /// If this is a subrange (array, vector, etc.), we need to temporarily store the the upper bound of the range.
    pub range_upper_bound: i64,
}

impl SvdVariable {
    pub fn new(name: SvdVariableName, type_name: SvdVariableType) -> SvdVariable {
        SvdVariable {
            variable_key: ObjectRef::default(),
            parent_key: ObjectRef::default(),
            name,
            value: SvdVariableValue::default(),
            type_name,
            variable_node_type: SvdVariableNodeType::default(),
            memory_location: SvdVariableLocation::default(),
            range_lower_bound: 0,
            range_upper_bound: 0,
        }
    }

    /// Get a unique key for this variable.
    pub fn variable_key(&self) -> ObjectRef {
        self.variable_key
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self, variable_cache: &SvdVariableCache) -> String {
        // Allow for chained `if let` without complaining
        if SvdVariableNodeType::SvdRegister == self.variable_node_type {
            if let SvdVariableValue::Valid(register_value) = &self.value {
                if let Ok(register_u32_value) = register_value.parse::<u32>() {
                    format!(
                        "{:032b} @ {:#010X}",
                        register_u32_value,
                        self.memory_location.memory_address().unwrap_or(u64::MAX) // We should never encounter a memory location that is invalid if we already used it to read the register value.
                    )
                } else {
                    format!("Invalid register value {register_value}")
                }
            } else {
                format!("{}", self.value)
            }
        } else if SvdVariableNodeType::SvdField == self.variable_node_type {
            // In this special case, we extract just the bits we need from the stored value of the register.
            if let SvdVariableValue::Valid(register_value) = &self.value {
                if let Ok(register_u32_value) = register_value.parse::<u32>() {
                    let mut bit_value: u32 = register_u32_value;
                    bit_value <<= 32 - self.range_upper_bound;
                    bit_value >>= 32 - (self.range_upper_bound - self.range_lower_bound);
                    format!(
                        "{:0width$b} @ {:#010X}:{}..{}",
                        bit_value,
                        self.memory_location.memory_address().unwrap_or(u64::MAX),
                        self.range_lower_bound,
                        self.range_upper_bound,
                        width = self.subrange_bounds().count()
                    )
                } else {
                    format!(
                        "Invalid bit range {}..{} from value {}",
                        self.range_lower_bound, self.range_upper_bound, register_value
                    )
                }
            } else {
                format!("{}", self.value)
            }
        } else if !self.value.is_empty() {
            // The `value` for this `Variable` is non empty because ...
            // - It is base data type for which a value was determined based on the core runtime, or ...
            // - We encountered an error somewhere, so report it to the user
            format!("{}", self.value)
        } else {
            // We need to construct a 'human readable' value using `fmt::Display` to represent the values of complex types and pointers.
            match variable_cache.has_children(self) {
                Ok(true) => self.formatted_variable_value(variable_cache, 0_usize, false),
                Ok(false) => {
                    if !self.memory_location.valid() {
                        // This condition should only be true for intermediate nodes from DWARF. These should not show up in the final `VariableCache`
                        // If a user sees this error, then there is a logic problem in the stack unwind
                        "Error: This is a bug! Attempted to evaluate a Variable with no type or no memory location".to_string()
                    } else {
                        format!(
                            "Unimplemented: Evaluate type {:?} at location 0x{:08x?}",
                            self.type_name, self.memory_location
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

    pub fn extract_value(&mut self, memory: &mut dyn MemoryInterface) {
        if let SvdVariableValue::Error(_) = self.value {
            // Nothing more to do ...
            return;
        }

        if self.variable_node_type == SvdVariableNodeType::SvdRegister
            || self.variable_node_type == SvdVariableNodeType::SvdField
        {
            // Special handling for SVD registers.
            // Because we cache the SVD structure once per sesion, we have to re-read the actual register values whenever queried.
            match memory.read_word_32(self.memory_location.memory_address().unwrap_or(u64::MAX)) {
                Ok(u32_value) => {
                    self.value = SvdVariableValue::Valid(u32_value.to_le().to_string())
                }
                Err(error) => {
                    self.value = SvdVariableValue::Error(format!(
                        "Unable to read peripheral register value @ {:#010X} : {:?}",
                        self.memory_location.memory_address().unwrap_or(u64::MAX),
                        error
                    ))
                }
            }
            return;
        }

        todo!("Implement `Variable::extract_value` for SvdVariable");
    }

    pub(crate) fn subrange_bounds(&self) -> Range<i64> {
        self.range_lower_bound..self.range_upper_bound
    }

    fn formatted_variable_value(
        &self,
        variable_cache: &SvdVariableCache,
        indentation: usize,
        show_name: bool,
    ) -> String {
        let line_feed = if indentation == 0 { "" } else { "\n" }.to_string();
        // Allow for chained `if let` without complaining
        #[allow(clippy::if_same_then_else)]
        if !self.value.is_empty() {
            if show_name {
                // Use the supplied value or error message.
                format!(
                    "{}{:\t<indentation$}{}: {} = {}",
                    line_feed, "", self.name, self.type_name, self.value
                )
            } else {
                // Use the supplied value or error message.
                format!("{}{:\t<indentation$}{}", line_feed, "", self.value)
            }
        } else {
            // Infer a human readable value using the available children of this variable.
            let mut compound_value = String::new();
            if let Ok(children) = variable_cache.get_children(self.variable_key) {
                // Generic handling of other structured types.
                // The pre- and post- fix is determined by the type of children.
                // compound_value = format!("{} {}", compound_value, self.type_name);

                if children.is_empty() {
                    // Struct with no children -> just print type name
                    // This is for example the None value of an Option.

                    format!("{}{:\t<indentation$}{}", line_feed, "", self.name)
                } else {
                    let (mut pre_fix, mut post_fix): (Option<String>, Option<String>) =
                        (None, None);

                    let mut child_count: usize = 0;

                    let mut is_tuple = false;

                    for child in children.iter() {
                        child_count += 1;
                        if pre_fix.is_none() && post_fix.is_none() {
                            if let SvdVariableName::Named(child_name) = &child.name {
                                if child_name.starts_with("__0") {
                                    is_tuple = true;
                                    // Treat this structure as a tuple
                                    pre_fix = Some(format!(
                                        "{}{:\t<indentation$}{}: {}({}) = {}(",
                                        line_feed,
                                        "",
                                        self.name,
                                        self.type_name,
                                        child.type_name,
                                        self.type_name,
                                    ));
                                    post_fix =
                                        Some(format!("{}{:\t<indentation$})", line_feed, ""));
                                } else {
                                    // Treat this structure as a `struct`

                                    if show_name {
                                        pre_fix = Some(format!(
                                            "{}{:\t<indentation$}{}: {} = {} {{",
                                            line_feed,
                                            "",
                                            self.name,
                                            self.type_name,
                                            self.type_name,
                                        ));
                                    } else {
                                        pre_fix = Some(format!(
                                            "{}{:\t<indentation$}{} {{",
                                            line_feed, "", self.type_name,
                                        ));
                                    }
                                    post_fix =
                                        Some(format!("{}{:\t<indentation$}}}", line_feed, ""));
                                }
                            };
                            if let Some(pre_fix) = &pre_fix {
                                compound_value = format!("{compound_value}{pre_fix}");
                            };
                        }

                        let print_name = !is_tuple;

                        compound_value = format!(
                            "{}{}{}",
                            compound_value,
                            child.formatted_variable_value(
                                variable_cache,
                                indentation + 1,
                                print_name
                            ),
                            if child_count == children.len() {
                                // Do not add a separator at the end of the list
                                ""
                            } else {
                                ", "
                            }
                        );
                    }
                    if let Some(post_fix) = &post_fix {
                        compound_value = format!("{compound_value}{post_fix}");
                    };
                    compound_value
                }
            } else {
                // We don't have a value, and we can't generate one from children values, so use the type_name
                format!("{:\t<indentation$}{}", "", self.type_name)
            }
        }
    }

    /// `true` if the Variable has a valid value, or an empty value.
    /// `false` if the Variable has a VariableValue::Error(_)value
    pub fn is_valid(&self) -> bool {
        self.value.is_valid()
    }

    /// The variable is considered to be an 'indexed' variable if the name starts with two underscores followed by a number. e.g. "__1".
    /// TODO: Consider replacing this logic with `std::str::pattern::Pattern` when that API stabilizes
    pub fn is_indexed(&self) -> bool {
        match &self.name {
            SvdVariableName::Named(name) => {
                name.starts_with("__")
                    && name
                        .find(char::is_numeric)
                        .map_or(false, |zero_based_position| zero_based_position == 2)
            }
            // Other kind of variables are never indexed
            _ => false,
        }
    }

    /// Implementing set_value(), because the library passes errors into the value of the variable.
    /// This ensures debug front ends can see the errors, but doesn't fail because of a single variable not being able to decode correctly.
    pub fn set_value(&mut self, new_value: SvdVariableValue) {
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
            self.value = SvdVariableValue::Error(format!("{} : {}", self.value, new_value));
        }
        if !self.value.is_valid() {
            // If the value is invalid, then make sure we don't propogate invalid memory location values.
            self.memory_location = SvdVariableLocation::Unavailable;
        }
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum SvdVariableNodeType {
    DoNotRecurse,
    SvdRegister,
    SvdField,
    SvdPeripheral,
    #[default]
    RecurseToBaseType,
}

/// The variants of VariableType allows us to streamline the conditional logic that requires specific handling depending on the nature of the variable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SvdVariableType {
    /// For infrequently used categories of variables that does not fall into any of the other `VariableType` variants.
    Other(String),
}

impl std::fmt::Display for SvdVariableType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvdVariableType::Other(other) => other.fmt(f),
        }
    }
}

/// Location of a variable
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum SvdVariableLocation {
    /// Location of the variable is not known. This means that it has not been evaluated yet.
    #[default]
    Unknown,
    /// The variable does not have a location currently, probably due to optimisations.
    Unavailable,
    /// The variable can be found in memory, at this address.
    Address(u64),
}

impl SvdVariableLocation {
    /// Return the memory address, if available. Otherwise an error is returned.
    pub fn memory_address(&self) -> Result<u64, DebugError> {
        match self {
            SvdVariableLocation::Address(address) => Ok(*address),
            other => Err(DebugError::UnwindIncompleteResults {
                message: format!("Variable does not have a memory location: location={other:?}"),
            }),
        }
    }

    /// Check if the location is valid, ie. not an error, unsupported, or unavailable.
    pub fn valid(&self) -> bool {
        match self {
            SvdVariableLocation::Address(_) | SvdVariableLocation::Unknown => true,
            _other => false,
        }
    }
}

impl std::fmt::Display for SvdVariableLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvdVariableLocation::Unknown => "<unknown value>".fmt(f),
            SvdVariableLocation::Unavailable => "<value not available>".fmt(f),
            SvdVariableLocation::Address(address) => write!(f, "{address:#010X}"),
        }
    }
}

/// The type of variable we have at hand.
#[derive(Debug, PartialEq, Eq, Clone, Serialize)]
pub enum SvdVariableName {
    /// Top-level variable for CMSIS-SVD file Device peripherals/registers/fields.
    PeripheralScopeRoot,
    /// Variable with a specific name
    Named(String),
}

impl std::fmt::Display for SvdVariableName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvdVariableName::PeripheralScopeRoot => write!(f, "Peripheral Variable"),
            SvdVariableName::Named(name) => name.fmt(f),
        }
    }
}

/// A [Variable] will have either a valid value, or some reason why a value could not be constructed.
/// - If we encounter expected errors, they will be displayed to the user as defined below.
/// - If we encounter unexpected errors, they will be treated as proper errors and will propagated to the calling process as an `Err()`
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum SvdVariableValue {
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
    #[default]
    Empty,
}

impl std::fmt::Display for SvdVariableValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SvdVariableValue::Valid(value) => value.fmt(f),
            SvdVariableValue::Error(error) => write!(f, "< {error} >",),
            SvdVariableValue::Empty => write!(
                f,
                "Value not set. Please use Variable::get_value() to infer a human readable variable value"
            ),
        }
    }
}

impl SvdVariableValue {
    /// Returns `true` if the variable resolver did not encounter an error, `false` otherwise.
    pub fn is_valid(&self) -> bool {
        !matches!(self, SvdVariableValue::Error(_))
    }

    /// Returns `true` if no value or error is present, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        matches!(self, SvdVariableValue::Empty)
    }
}
