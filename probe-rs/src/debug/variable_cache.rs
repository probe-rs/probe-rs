use super::*;
use crate::Error;
use anyhow::anyhow;
use gimli::{UnitOffset, UnitSectionOffset};
use serde::{Serialize, Serializer};
use std::collections::HashMap;

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableCache {
    root_variable_key: ObjectRef,

    variable_hash_map: HashMap<ObjectRef, Variable>,
}

impl Serialize for VariableCache {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeStruct;

        /// This is a modified version of the [`Variable`] struct, to be used for serialization as a recursive tree node.
        #[derive(Serialize)]
        struct VariableTreeNode {
            name: VariableName,
            type_name: VariableType,
            /// To eliminate noise, we will only show values for base data types and strings.
            value: String,
            /// ONLY If there are children.
            #[serde(skip_serializing_if = "Vec::is_empty")]
            children: Vec<VariableTreeNode>,
        }

        fn recurse_cache(variable_cache: &VariableCache) -> VariableTreeNode {
            let root_node = variable_cache.root_variable();

            VariableTreeNode {
                name: root_node.name.clone(),
                type_name: root_node.type_name.clone(),
                value: root_node.get_value(variable_cache),
                children: if root_node.range_upper_bound > 50 {
                    // Empty Vec's will show as variables with no children.
                    Vec::new()
                } else {
                    recurse_variables(variable_cache, root_node.variable_key)
                },
            }
        }

        /// A helper function to recursively build the variable tree with `VariableTreeNode` entries.
        fn recurse_variables(
            variable_cache: &VariableCache,
            parent_variable_key: ObjectRef,
        ) -> Vec<VariableTreeNode> {
            variable_cache
                .get_children(parent_variable_key)
                .unwrap()
                .into_iter()
                .map(|child_variable: Variable| {
                    let value = if child_variable.range_upper_bound > 50 {
                        format!("Data types with more than 50 members are excluded from this output. This variable has {} child members.", child_variable.range_upper_bound)
                    } else {
                        child_variable.get_value(variable_cache)
                    };

                    VariableTreeNode {
                                    name: child_variable.name,
                                    type_name: child_variable.type_name,
                                    value ,
                                    children: if child_variable.range_upper_bound > 50 {
                                        // Empty Vec's will show as variables with no children.
                                        Vec::new()
                                    } else {
                                        recurse_variables(variable_cache, child_variable.variable_key)
                                    },
                                                    }
                })
                .collect::<Vec<VariableTreeNode>>()
        }

        let mut state = serializer.serialize_struct("Variables", 1)?;
        state.serialize_field("Child Variables", &recurse_cache(self))?;
        state.end()
    }
}

impl VariableCache {
    fn new(mut variable: Variable) -> Self {
        let key = get_object_reference();

        variable.variable_key = key;

        let cache = VariableCache {
            root_variable_key: key,
            variable_hash_map: HashMap::from([(key, variable)]),
        };

        cache
    }

    /// Create a cache for static variables for the given unit
    pub fn new_static_cache(header_offset: UnitSectionOffset, entries_offset: UnitOffset) -> Self {
        let mut static_root_variable =
            Variable::new(header_offset.as_debug_info_offset(), Some(entries_offset));
        static_root_variable.variable_node_type = VariableNodeType::DirectLookup;
        static_root_variable.name = VariableName::StaticScopeRoot;

        VariableCache::new(static_root_variable)
    }

    /// Create a cache for local variables for the given DIE
    pub fn new_local_cache(header_offset: UnitSectionOffset, entries_offset: UnitOffset) -> Self {
        let mut local_root_variable =
            Variable::new(header_offset.as_debug_info_offset(), Some(entries_offset));
        local_root_variable.variable_node_type = VariableNodeType::DirectLookup;
        local_root_variable.name = VariableName::LocalScopeRoot;

        VariableCache::new(local_root_variable)
    }

    /// Create a new cache for SVD variables
    pub fn new_svd_cache() -> Self {
        let mut device_root_variable = Variable::new(None, None);
        device_root_variable.variable_node_type = VariableNodeType::DoNotRecurse;
        device_root_variable.name = VariableName::PeripheralScopeRoot;

        VariableCache::new(device_root_variable)
    }

    /// Get the root variable of the cache
    pub fn root_variable(&self) -> Variable {
        self.variable_hash_map[&self.root_variable_key].clone()
    }

    /// Returns the number of `Variable`s in the cache.
    // These caches are constructed with a single root variable, so this should never be empty.
    pub fn len(&self) -> usize {
        self.variable_hash_map.len()
    }

    /// Returns `true` if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.variable_hash_map.is_empty()
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
        parent_key: ObjectRef,
        cache_variable: Variable,
        memory: &mut dyn MemoryInterface,
    ) -> Result<Variable, Error> {
        let mut variable_to_add = cache_variable.clone();
        // Validate that the parent_key exists ...
        if self.variable_hash_map.contains_key(&parent_key) {
            variable_to_add.parent_key = parent_key;
        } else {
            return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", variable_to_add.name, parent_key).into());
        }

        // Is this an *add* or *update* operation?
        let stored_key = if variable_to_add.variable_key == ObjectRef::Invalid {
            // The caller is telling us this is definitely a new `Variable`
            variable_to_add.variable_key = get_object_reference();

            tracing::trace!(
                "VariableCache: Add Variable: key={:?}, parent={:?}, name={:?}",
                variable_to_add.variable_key,
                variable_to_add.parent_key,
                &variable_to_add.name
            );

            let new_entry_key = variable_to_add.variable_key;
            if let Some(old_variable) = self
                .variable_hash_map
                .insert(variable_to_add.variable_key, variable_to_add)
            {
                return Err(anyhow!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", cache_variable.name, old_variable.variable_key).into());
            }
            new_entry_key
        } else {
            // Attempt to update an existing `Variable` in the cache
            tracing::trace!(
                "VariableCache: Update Variable, key={:?}, name={:?}",
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
                return Err(anyhow!("Attempt to update and existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, variable_to_add.variable_key).into());
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
                stored_variable.extract_value(memory, self);
            }
            if self
                .variable_hash_map
                .insert(stored_variable.variable_key, stored_variable.clone())
                .is_none()
            {
                Err(anyhow!("Failed to store variable at variable_cache_key: {:?}. Please report this as a bug.", stored_key).into())
            } else {
                Ok(stored_variable)
            }
        } else {
            Err(anyhow!(
                "Failed to store variable at variable_cache_key: {:?}. Please report this as a bug.",
                stored_key
            )
            .into())
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `variable_key`.
    pub fn get_variable_by_key(&self, variable_key: ObjectRef) -> Option<Variable> {
        self.variable_hash_map.get(&variable_key).cloned()
    }

    /// Retrieve a clone of a specific `Variable`, using the `name` and `parent_key`.
    /// If there is more than one, it will be logged (tracing::error!), and only the last will be returned.
    pub fn get_variable_by_name_and_parent(
        &self,
        variable_name: &VariableName,
        parent_key: ObjectRef,
    ) -> Option<Variable> {
        let mut child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                &child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .collect::<Vec<&Variable>>();

        child_variables.sort_by_key(|v| v.variable_key);

        match &child_variables[..] {
            [] => None,
            [variable] => Some((*variable).clone()),
            [.., last] => {
                tracing::error!("Found {} variables with parent_key={:?} and name={}. Please report this as a bug.", child_variables.len(), parent_key, variable_name);
                Some((*last).clone())
            }
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &VariableName) -> Option<Variable> {
        let mut child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name))
            .collect::<Vec<&Variable>>();

        // Sort the variables by key, so that we can consistently return the first one.
        // The hash map does not return the values in a consistent order.
        child_variables.sort_by_key(|v| v.variable_key);

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

    /// Retrieve `clone`d version of all the children of a `Variable`.
    /// If `parent_key == None`, it will return all the top level variables (no parents) in this cache.
    pub fn get_children(&self, parent_key: ObjectRef) -> Result<Vec<Variable>, Error> {
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
        self.get_children(parent_variable.variable_key)
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
                    search_variable.parent_key == obsolete_child_variable.variable_key
                })
                .for_each(|grand_child| grand_child.parent_key = parent_variable.variable_key);
            // Remove the intermediate variable from the cache
            self.remove_cache_entry(obsolete_child_variable.variable_key)?;
        }
        Ok(())
    }

    /// Removing an entry's children from the `VariableCache` will recursively remove all their children
    pub fn remove_cache_entry_children(
        &mut self,
        parent_variable_key: ObjectRef,
    ) -> Result<(), Error> {
        let children = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_variable_key)
            .cloned()
            .collect::<Vec<Variable>>();

        for child in children {
            self.remove_cache_entry(child.variable_key)?;
        }

        Ok(())
    }
    /// Removing an entry from the `VariableCache` will recursively remove all its children
    pub fn remove_cache_entry(&mut self, variable_key: ObjectRef) -> Result<(), Error> {
        self.remove_cache_entry_children(variable_key)?;
        if self.variable_hash_map.remove(&variable_key).is_none() {
            return Err(anyhow!("Failed to remove a `VariableCache` entry with key: {:?}. Please report this as a bug.", variable_key).into());
        };
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use gimli::{DebugInfoOffset, UnitOffset};
    use termtree::Tree;

    use crate::{
        debug::{
            Variable, VariableCache, VariableLocation, VariableName, VariableNodeType,
            VariableType, VariantRole,
        },
        test::MockMemory,
    };

    fn show_tree(cache: &VariableCache) {
        let tree = build_tree(cache, cache.root_variable());

        println!("{}", tree);
    }

    fn build_tree(cache: &VariableCache, variable: Variable) -> Tree<String> {
        let mut entry = Tree::new(format!(
            "{:?}: name={:?}, type={:?}, value={:?}",
            variable.variable_key,
            variable.name,
            variable.type_name,
            variable.get_value(cache)
        ));

        let children = cache.get_children(variable.variable_key).unwrap();

        for child in children {
            entry.push(build_tree(cache, child));
        }

        entry
    }

    #[test]
    fn static_cache() {
        let c = VariableCache::new_static_cache(DebugInfoOffset(0).into(), UnitOffset(0));

        let cache_variable = c.root_variable();

        println!("{:#?}", cache_variable);

        //assert_eq!(cache_variable.parent_key, None);
        assert_eq!(cache_variable.name, VariableName::StaticScopeRoot);
        assert_eq!(cache_variable.type_name, VariableType::Unknown);
        assert_eq!(
            cache_variable.variable_node_type,
            VariableNodeType::DirectLookup
        );

        assert_eq!(cache_variable.get_value(&c), "Unknown");

        assert_eq!(cache_variable.source_location, None);
        assert_eq!(cache_variable.memory_location, VariableLocation::Unknown);
        assert_eq!(cache_variable.byte_size, None);
        assert_eq!(cache_variable.member_index, None);
        assert_eq!(cache_variable.range_lower_bound, 0);
        assert_eq!(cache_variable.range_upper_bound, 0);
        assert_eq!(cache_variable.role, VariantRole::NonVariant);
    }

    #[test]
    fn find_children() {
        let mut memory = MockMemory::new();

        let mut cache = VariableCache::new_svd_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = Variable::new(None, None);
        let var_1 = cache.cache_variable(root_key, var_1, &mut memory).unwrap();

        let var_2 = Variable::new(None, None);
        let var_2 = cache.cache_variable(root_key, var_2, &mut memory).unwrap();

        let children = cache.get_children(root_key).unwrap();

        let expected_children = vec![var_1, var_2];

        assert_eq!(children, expected_children);
    }

    #[test]
    fn find_entry() {
        let mut memory = MockMemory::new();

        let mut cache = VariableCache::new_svd_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = Variable::new(None, None);
        let var_1 = cache.cache_variable(root_key, var_1, &mut memory).unwrap();

        let var_2 = Variable::new(None, None);
        let _var_2 = cache.cache_variable(root_key, var_2, &mut memory).unwrap();

        assert_eq!(cache.get_variable_by_key(var_1.variable_key), Some(var_1));
    }

    /// Build up a tree like this:
    ///
    /// [root]
    /// |
    /// +-- [var_1]
    /// +-- [var_2]
    /// |   |
    /// |   +-- [var_3]
    /// |   |   |
    /// |   |   +-- [var_5]
    /// |   |
    /// |   +-- [var_4]
    /// |
    /// +-- [var_6]
    ///     |
    ///     +-- [var_7]
    fn build_test_tree() -> (VariableCache, Vec<Variable>) {
        let mut memory = MockMemory::new();

        let mut cache = VariableCache::new_svd_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = Variable::new(None, None);
        let var_1 = cache.cache_variable(root_key, var_1, &mut memory).unwrap();

        let var_2 = Variable::new(None, None);
        let var_2 = cache.cache_variable(root_key, var_2, &mut memory).unwrap();

        let var_3 = Variable::new(None, None);
        let var_3 = cache
            .cache_variable(var_2.variable_key, var_3, &mut memory)
            .unwrap();

        let var_4 = Variable::new(None, None);
        let var_4 = cache
            .cache_variable(var_2.variable_key, var_4, &mut memory)
            .unwrap();

        let var_5 = Variable::new(None, None);
        let var_5 = cache
            .cache_variable(var_3.variable_key, var_5, &mut memory)
            .unwrap();

        let var_6 = Variable::new(None, None);
        let var_6 = cache.cache_variable(root_key, var_6, &mut memory).unwrap();

        let var_7 = Variable::new(None, None);
        let var_7 = cache
            .cache_variable(var_6.variable_key, var_7, &mut memory)
            .unwrap();

        assert_eq!(cache.len(), 8);

        let variables = vec![
            cache.root_variable(),
            var_1,
            var_2,
            var_3,
            var_4,
            var_5,
            var_6,
            var_7,
        ];

        (cache, variables)
    }

    #[test]
    fn remove_entry() {
        let (mut cache, vars) = build_test_tree();

        // no children
        cache.remove_cache_entry(vars[1].variable_key).unwrap();
        assert!(cache.get_variable_by_key(vars[1].variable_key).is_none());
        assert_eq!(cache.len(), 7);

        // one child
        cache.remove_cache_entry(vars[6].variable_key).unwrap();
        assert!(cache.get_variable_by_key(vars[6].variable_key).is_none());
        assert!(cache.get_variable_by_key(vars[7].variable_key).is_none());
        assert_eq!(cache.len(), 5);

        // multi-level children
        cache.remove_cache_entry(vars[2].variable_key).unwrap();
        assert!(cache.get_variable_by_key(vars[2].variable_key).is_none());
        assert!(cache.get_variable_by_key(vars[3].variable_key).is_none());
        assert!(cache.get_variable_by_key(vars[4].variable_key).is_none());
        assert!(cache.get_variable_by_key(vars[5].variable_key).is_none());
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn find_entry_by_name() {
        let mut memory = MockMemory::new();

        let (mut cache, mut vars) = build_test_tree();
        let non_unique_name = VariableName::Named("non_unique_name".to_string());
        let unique_name = VariableName::Named("unique_name".to_string());

        show_tree(&cache);

        vars[3].name = non_unique_name.clone();
        vars[3] = cache
            .cache_variable(vars[3].parent_key, vars[3].clone(), &mut memory)
            .unwrap();

        show_tree(&cache);

        vars[4].name = unique_name.clone();
        vars[4] = cache
            .cache_variable(vars[4].parent_key, vars[4].clone(), &mut memory)
            .unwrap();

        show_tree(&cache);

        vars[6].name = non_unique_name.clone();
        vars[6] = cache
            .cache_variable(vars[6].parent_key, vars[6].clone(), &mut memory)
            .unwrap();

        show_tree(&cache);

        assert!(vars[3].variable_key < vars[6].variable_key);

        let var_3 = cache.get_variable_by_name(&non_unique_name).unwrap();
        assert_eq!(&var_3, &vars[3]);

        let var_4 = cache.get_variable_by_name(&unique_name).unwrap();
        assert_eq!(&var_4, &vars[4]);

        let var_6 = cache
            .get_variable_by_name_and_parent(&non_unique_name, vars[6].parent_key)
            .unwrap();
        assert_eq!(&var_6, &vars[6]);
    }

    #[test]
    fn adopt_grand_children() {
        let (mut cache, mut vars) = build_test_tree();

        cache.adopt_grand_children(&vars[2], &vars[3]).unwrap();

        assert!(cache.get_variable_by_key(vars[3].variable_key).is_none());

        let new_children = cache.get_children(vars[2].variable_key).unwrap();

        vars[5].parent_key = vars[2].variable_key;

        assert_eq!(new_children, vec![vars[4].clone(), vars[5].clone()]);
    }
}
