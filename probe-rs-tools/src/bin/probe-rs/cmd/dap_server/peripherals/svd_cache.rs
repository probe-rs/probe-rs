use std::collections::BTreeMap;

use probe_rs::debug::{get_object_reference, DebugError, ObjectRef};
use probe_rs::MemoryInterface;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug, Clone, PartialEq)]
pub struct SvdVariableCache {
    root_variable_key: ObjectRef,

    variable_hash_map: BTreeMap<ObjectRef, Variable>,
}

impl SvdVariableCache {
    /// Create a new cache for SVD variables
    pub fn new_svd_cache() -> Self {
        let root_variable_key = get_object_reference();
        let root_variable = Variable {
            variable_key: root_variable_key,
            parent_key: ObjectRef::Invalid,
            name: "Peripheral variable".to_string(),
            variable_kind: SvdVariable::Root,
        };

        SvdVariableCache {
            root_variable_key,
            variable_hash_map: BTreeMap::from([(root_variable_key, root_variable)]),
        }
    }

    /// Retrieve all the children of a `Variable`.
    /// If `parent_key == None`, it will return all the top level variables (no parents) in this cache.
    pub fn get_children(&self, parent_key: ObjectRef) -> Vec<&Variable> {
        let children = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_key)
            .collect::<Vec<_>>();

        children
    }

    /// Retrieve a specific `Variable`, using the `variable_key`.
    pub fn get_variable_by_key(&self, variable_key: ObjectRef) -> Option<&Variable> {
        self.variable_hash_map.get(&variable_key)
    }

    /// Get the root variable of the cache
    pub fn root_variable_key(&self) -> ObjectRef {
        self.root_variable_key
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &str) -> Option<&Variable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name))
            .collect::<Vec<&Variable>>();

        match &child_variables[..] {
            [] => None,
            [variable] => Some(*variable),
            [first, ..] => {
                tracing::warn!(
                    "Found {} variables with name={}. Please report this as a bug.",
                    child_variables.len(),
                    variable_name
                );
                Some(*first)
            }
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `name` and `parent_key`.
    /// If there is more than one, it will be logged (tracing::error!), and only the last will be returned.
    pub fn get_variable_by_name_and_parent(
        &self,
        variable_name: &str,
        parent_key: ObjectRef,
    ) -> Option<&Variable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .collect::<Vec<&Variable>>();

        match &child_variables[..] {
            [] => None,
            [variable] => Some(variable),
            [.., last] => {
                tracing::error!("Found {} variables with parent_key={:?} and name={}. Please report this as a bug.", child_variables.len(), parent_key, variable_name);
                Some(last)
            }
        }
    }

    pub fn add_variable(
        &mut self,
        parent_key: ObjectRef,
        name: String,
        variable: SvdVariable,
    ) -> Result<ObjectRef, DebugError> {
        let cache_variable = {
            let variable_key = get_object_reference();
            Variable {
                variable_key,
                parent_key,
                name,
                variable_kind: variable,
            }
        };

        // Validate that the parent_key exists ...
        if !self.variable_hash_map.contains_key(&parent_key) {
            return Err(DebugError::Other(format!("SvdVariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", cache_variable.name, parent_key)));
        }

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
            return Err(DebugError::Other(format!("Attempt to insert a new `SvdVariable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", cache_variable.name, old_variable.variable_key)));
        }

        Ok(cache_variable.variable_key)
    }
}

/// The `Variable` struct is used in conjunction with `VariableCache` to cache data about variables.
///
/// Any modifications to the `Variable` value will be transient (lost when it goes out of scope),
/// unless it is updated through one of the available methods on `VariableCache`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    /// Every variable must have a unique key value assigned to it. The value will be zero until it is stored in VariableCache, at which time its value will be set to the same as the VariableCache::variable_cache_key
    variable_key: ObjectRef,
    /// Every variable must have a unique parent assigned to it when stored in the VariableCache.
    parent_key: ObjectRef,
    /// The name of the SVD variable, the name of a register, field, peripheral, etc.
    name: String,

    pub variable_kind: SvdVariable,
}

impl Variable {
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get a unique key for this variable.
    pub fn variable_key(&self) -> ObjectRef {
        self.variable_key
    }

    /// Memory reference, compatible with DAP
    pub fn memory_reference(&self) -> Option<String> {
        match self.variable_kind {
            SvdVariable::SvdRegister {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{:#010X}", address)),

            SvdVariable::SvdField {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{:#010X}", address)),
            _ => None,
        }
    }

    pub fn type_name(&self) -> Option<String> {
        self.variable_kind.type_name()
    }

    /// Value of the variable, compatible with DAP
    ///
    /// The value might be retrieved using the `MemoryInterface` to read the value from the target.
    pub fn get_value(&self, memory: &mut dyn MemoryInterface) -> String {
        self.variable_kind.get_value(memory)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvdVariable {
    Root,
    /// Register with address
    SvdRegister {
        address: u64,
        /// true if the register is write-only or  if a ride has side effects
        restricted_read: bool,

        /// Description of the register, used as type in DAP
        description: Option<String>,
    },
    /// Field with address
    SvdField {
        address: u64,
        /// true if the register is write-only or  if a ride has side effects
        restricted_read: bool,
        bit_range_lower_bound: u32,
        bit_range_upper_bound: u32,

        description: Option<String>,
    },
    /// Peripheral with peripheral base address
    SvdPeripheral {
        base_address: u64,
        description: Option<String>,
    },
    SvdPeripheralGroup {
        description: Option<String>,
    },
}

impl SvdVariable {
    fn get_value(&self, memory: &mut dyn MemoryInterface) -> String {
        match &self {
            SvdVariable::Root => "".to_string(),
            // For peripheral and peripheral group, we use the description as the value if there is one, otherwise there is no value
            SvdVariable::SvdPeripheral { description, .. }
            | SvdVariable::SvdPeripheralGroup { description } => {
                description.as_ref().cloned().unwrap_or_default()
            }

            SvdVariable::SvdRegister {
                address,
                restricted_read,
                ..
            } => {
                if *restricted_read {
                    format!(
                        "Register cannot be read without side effects @ {:#010X}",
                        address
                    )
                } else {
                    let value = match memory.read_word_32(*address) {
                        Ok(u32_value) => Ok(u32_value),
                        Err(error) => Err(format!(
                            "Unable to read peripheral register value @ {:#010X} : {:?}",
                            address, error
                        )),
                    };

                    match value {
                        Ok(u32_value) => {
                            format!("{:#010X}", u32_value)
                        }
                        Err(error) => error,
                    }
                }
            }
            SvdVariable::SvdField {
                address,
                restricted_read,
                bit_range_lower_bound,
                bit_range_upper_bound,
                ..
            } => {
                if *restricted_read {
                    format!(
                        "Field cannot be read without side effects @ {:#010X}",
                        address
                    )
                } else {
                    let value = match memory.read_word_32(*address) {
                        Ok(u32_value) => Ok(u32_value),
                        Err(error) => Err(format!(
                            "Unable to read peripheral register field value @ {:#010X} : {:?}",
                            address, error
                        )),
                    };

                    // In this special case, we extract just the bits we need from the stored value of the register.
                    match value {
                        Ok(register_u32_value) => {
                            let mut bit_value: u32 = register_u32_value;
                            bit_value <<= 32 - bit_range_upper_bound;
                            bit_value >>= 32 - (bit_range_upper_bound - bit_range_lower_bound);
                            format!(
                                "{:0width$b} @ {:#010X}:{}..{}",
                                bit_value,
                                address,
                                bit_range_lower_bound,
                                bit_range_upper_bound,
                                width = (*bit_range_lower_bound..*bit_range_upper_bound).count()
                            )
                        }
                        Err(e) => e,
                    }
                }
            }
        }
    }

    fn type_name(&self) -> Option<String> {
        match &self {
            SvdVariable::SvdRegister { description, .. }
            | SvdVariable::SvdField { description, .. } => description.clone(),
            SvdVariable::SvdPeripheral { .. } => Some("Peripheral".to_string()),
            SvdVariable::SvdPeripheralGroup { .. } => Some("Peripheral Group".to_string()),
            SvdVariable::Root => None,
        }
    }
}
