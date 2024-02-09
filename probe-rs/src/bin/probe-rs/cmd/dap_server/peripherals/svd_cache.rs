use std::collections::BTreeMap;

use probe_rs::debug::{get_object_reference, ObjectRef};
use probe_rs::{Error, MemoryInterface};

use anyhow::anyhow;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug, Clone, PartialEq)]
pub struct SvdVariableCache {
    root_variable_key: ObjectRef,

    variable_hash_map: BTreeMap<ObjectRef, SvdVariable>,
}

impl SvdVariableCache {
    /// Create a new cache for SVD variables
    pub fn new_svd_cache() -> Self {
        let key = get_object_reference();

        let mut variable =
            SvdVariable::new("Peripheral variable".to_string(), SvdVariableNodeType::Root);
        variable.variable_key = key;

        SvdVariableCache {
            root_variable_key: key,
            variable_hash_map: BTreeMap::from([(key, variable)]),
        }
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

    /// Retrieve a specific `Variable`, using the `variable_key`.
    pub fn get_variable_by_key(&self, variable_key: ObjectRef) -> Option<&SvdVariable> {
        self.variable_hash_map.get(&variable_key)
    }

    /// Get the root variable of the cache
    pub fn root_variable_key(&self) -> ObjectRef {
        self.root_variable_key
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &str) -> Option<&SvdVariable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name))
            .collect::<Vec<&SvdVariable>>();

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
    ) -> Option<SvdVariable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                child_variable.name == variable_name && child_variable.parent_key == parent_key
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
        mut cache_variable: SvdVariable,
    ) -> Result<ObjectRef, Error> {
        // Validate that the parent_key exists ...
        if self.variable_hash_map.contains_key(&parent_key) || parent_key == self.root_variable_key
        {
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

        Ok(cache_variable.variable_key)
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
    pub name: String,

    /// For 'lazy loading' of certain variable types we have to determine if the variable recursion should be deferred, and if so, how to resolve it when the request for further recursion happens.
    /// See [VariableNodeType] for more information.
    pub variable_node_type: SvdVariableNodeType,
}

impl SvdVariable {
    pub fn new(name: String, variable_node_type: SvdVariableNodeType) -> SvdVariable {
        SvdVariable {
            variable_key: ObjectRef::default(),
            parent_key: ObjectRef::default(),
            name,
            variable_node_type,
        }
    }

    /// Get a unique key for this variable.
    pub fn variable_key(&self) -> ObjectRef {
        self.variable_key
    }

    /// Memory reference, compatible with DAP
    pub fn memory_reference(&self) -> Option<String> {
        match self.variable_node_type {
            SvdVariableNodeType::SvdRegister {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{:#010X}", address)),

            SvdVariableNodeType::SvdField {
                address,
                restricted_read: false,
                ..
            } => Some(format!("{:#010X}", address)),
            _ => None,
        }
    }

    pub fn type_name(&self) -> Option<String> {
        match &self.variable_node_type {
            SvdVariableNodeType::SvdRegister { description, .. }
            | SvdVariableNodeType::SvdField { description, .. } => description.clone(),
            SvdVariableNodeType::SvdPeripheral { .. } => Some("Peripheral".to_string()),
            SvdVariableNodeType::SvdPeripheralGroup { .. } => Some("Peripheral Group".to_string()),
            SvdVariableNodeType::Root => None,
        }
    }

    /// Implementing get_value(), because Variable.value has to be private (a requirement of updating the value without overriding earlier values ... see set_value()).
    pub fn get_value(&self, memory: &mut dyn MemoryInterface) -> String {
        match &self.variable_node_type {
            SvdVariableNodeType::Root => "".to_string(),
            SvdVariableNodeType::SvdPeripheral { description, .. }
            | SvdVariableNodeType::SvdPeripheralGroup { description } => description
                .as_ref()
                .cloned()
                .unwrap_or_else(|| self.name.to_string()),

            SvdVariableNodeType::SvdRegister {
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
            SvdVariableNodeType::SvdField {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SvdVariableNodeType {
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
