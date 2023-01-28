use super::*;
use crate::Error;
use anyhow::anyhow;
use std::collections::HashMap;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug)]
pub struct VariableCache {
    pub(crate) variable_hash_map: HashMap<i64, Variable>,
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

            tracing::trace!(
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
            tracing::trace!(
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
                    tracing::trace!("Updated:  {:?}", variable_to_add);
                    tracing::trace!("Previous: {:?}", prev_entry);
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
            if !(stored_variable.variable_node_type == VariableNodeType::SvdPeripheral
                || stored_variable.variable_node_type == VariableNodeType::SvdRegister
                || stored_variable.variable_node_type == VariableNodeType::SvdField)
            {
                // Only do this for non-SVD variables. Those will extract their value everytime they are read from the client.
                stored_variable.extract_value(core, self);
            }
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
    /// If there is more than one, it will be logged (tracing::error!), and only the last will be returned.
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
                tracing::error!("Found {} variables with parent_key={:?} and name={}. Please report this as a bug.", child_count, parent_key, variable_name);
                child_variables.last().cloned()
            }
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &VariableName) -> Option<Variable> {
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| &child_variable.name == variable_name)
            .cloned()
            .collect::<Vec<Variable>>();
        match child_variables.len() {
            0 => None,
            1 => child_variables.first().cloned(),
            child_count => {
                tracing::warn!(
                    "Found {} variables with name={}. Please report this as a bug.",
                    child_count,
                    variable_name
                );
                child_variables.first().cloned()
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
        if obsolete_child_variable.type_name == VariableType::Unknown
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
