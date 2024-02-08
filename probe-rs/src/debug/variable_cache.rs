use super::*;
use crate::{debug::stack_frame::StackFrameInfo, Error};
use anyhow::anyhow;
use gimli::{DebugInfoOffset, UnitOffset, UnitSectionOffset};
use probe_rs_target::MemoryRange;
use serde::{Serialize, Serializer};
use std::{
    collections::{btree_map::Entry, BTreeMap},
    ops::Range,
};

/// VariableCache stores available `Variable`s, and provides methods to create and navigate the parent-child relationships of the Variables.
#[derive(Debug, Clone, PartialEq)]
pub struct VariableCache {
    root_variable_key: ObjectRef,

    variable_hash_map: BTreeMap<ObjectRef, Variable>,
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

        VariableCache {
            root_variable_key: key,
            variable_hash_map: BTreeMap::from([(key, variable)]),
        }
    }

    /// Create a variable cache based on DWARF debug information
    ///
    /// The `header_offset` and `entries_offset` values are used to
    /// extract the variable information from the debug information.
    ///
    /// The entries form a tree, only entries below the entry
    /// at `entries_offset` are considered when filling the cache.
    pub fn new_dwarf_cache(
        header_offset: UnitSectionOffset,
        entries_offset: UnitOffset,
        name: VariableName,
    ) -> Self {
        let mut static_root_variable =
            Variable::new(header_offset.as_debug_info_offset(), Some(entries_offset));
        static_root_variable.variable_node_type = VariableNodeType::DirectLookup;
        static_root_variable.name = name;

        VariableCache::new(static_root_variable)
    }

    /// Get the root variable of the cache
    pub fn root_variable(&self) -> Variable {
        self.variable_hash_map[&self.root_variable_key].clone()
    }

    /// Get a mutable reference to the root variable of the cache
    pub fn root_variable_mut(&mut self) -> Option<&mut Variable> {
        self.variable_hash_map.get_mut(&self.root_variable_key)
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

    /// Create a new variable in the cache
    pub fn create_variable(
        &mut self,
        parent_key: ObjectRef,
        header_offset: Option<DebugInfoOffset>,
        entries_offset: Option<UnitOffset>,
    ) -> Result<Variable, Error> {
        let mut variable_to_add = Variable::new(header_offset, entries_offset);
        // Validate that the parent_key exists ...
        if self.variable_hash_map.contains_key(&parent_key) {
            variable_to_add.parent_key = parent_key;
        } else {
            return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", variable_to_add.name, parent_key).into());
        }

        // The caller is telling us this is definitely a new `Variable`
        variable_to_add.variable_key = get_object_reference();

        tracing::trace!(
            "VariableCache: Add Variable: key={:?}, parent={:?}, name={:?}",
            variable_to_add.variable_key,
            variable_to_add.parent_key,
            &variable_to_add.name
        );

        match self.variable_hash_map.entry(variable_to_add.variable_key) {
            Entry::Occupied(_) => {
                return Err(anyhow!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", variable_to_add.name, variable_to_add.variable_key).into());
            }
            Entry::Vacant(entry) => {
                entry.insert(variable_to_add.clone());
            }
        }

        Ok(variable_to_add)
    }

    /// Add a variable to the cache
    ///
    /// The parent key must exist in the cache, and the variable
    /// must not have a key assigned yet.
    pub fn add_variable(
        &mut self,
        parent_key: ObjectRef,
        cache_variable: &mut Variable,
    ) -> Result<(), Error> {
        // Validate that the parent_key exists ...
        if self.variable_hash_map.contains_key(&parent_key) {
            cache_variable.parent_key = parent_key;
        } else {
            return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", cache_variable.name, parent_key).into());
        }

        if cache_variable.variable_key != ObjectRef::Invalid {
            return Err(anyhow!("VariableCache: Attempted to add a new variable: {} with already set key: {:?}. Please report this as a bug", cache_variable.name, cache_variable.variable_key).into());
        }

        // The caller is telling us this is definitely a new `Variable`
        cache_variable.variable_key = get_object_reference();

        tracing::trace!(
            "VariableCache: Add Variable: key={:?}, parent={:?}, name={:?}",
            cache_variable.variable_key,
            cache_variable.parent_key,
            &cache_variable.name
        );

        if let Some(old_variable) = self
            .variable_hash_map
            .insert(cache_variable.variable_key, cache_variable.clone())
        {
            return Err(anyhow!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", cache_variable.name, old_variable.variable_key).into());
        }

        Ok(())
    }

    /// Update a variable in the cache,
    /// and update the value of the variable.
    pub fn update_variable_and_value(
        &mut self,
        cache_variable: &mut Variable,
        memory: &mut dyn MemoryInterface,
    ) -> Result<(), Error> {
        // Is this an *add* or *update* operation?
        if cache_variable.variable_key == ObjectRef::Invalid {
            return Err(anyhow!("Attempt to update an existing `Variable`:{:?} with an invalid cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key).into());
        }

        // Attempt to update an existing `Variable` in the cache
        tracing::trace!(
            "VariableCache: Update Variable, key={:?}, name={:?}",
            cache_variable.variable_key,
            &cache_variable.name
        );

        if let Some(prev_entry) = self.variable_hash_map.get_mut(&cache_variable.variable_key) {
            if cache_variable != prev_entry {
                tracing::trace!("Updated:  {:?}", cache_variable);
                tracing::trace!("Previous: {:?}", prev_entry);
            }

            *prev_entry = cache_variable.clone();
        } else {
            return Err(anyhow!("Attempt to update an existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key).into());
        }

        // As the final act, we need to update the variable with an appropriate value.
        // This requires distinct steps to ensure we don't get `borrow` conflicts on the variable cache.
        let Some(mut stored_variable) = self.get_variable_by_key(cache_variable.variable_key)
        else {
            return Err(anyhow!(
                "Failed to store variable at variable_cache_key: {:?}. Please report this as a bug.",
                cache_variable.variable_key
            )
            .into());
        };

        // Only do this for non-SVD variables. Those will extract their value everytime they are read from the client.
        stored_variable.extract_value(memory, self);

        if self
            .variable_hash_map
            .insert(stored_variable.variable_key, stored_variable.clone())
            .is_none()
        {
            Err(anyhow!("Failed to store variable at variable_cache_key: {:?}. Please report this as a bug.", cache_variable.variable_key).into())
        } else {
            *cache_variable = stored_variable;
            Ok(())
        }
    }

    /// Update a variable in the cache
    ///
    /// This function does not update the value of the variable.
    pub fn update_variable(&mut self, cache_variable: &Variable) -> Result<(), Error> {
        if cache_variable.variable_key == ObjectRef::Invalid {
            return Err(anyhow!("Attempt to update an existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key).into());
        }

        // Attempt to update an existing `Variable` in the cache
        tracing::trace!(
            "VariableCache: Update Variable, key={:?}, name={:?}",
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
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| {
                &child_variable.name == variable_name && child_variable.parent_key == parent_key
            })
            .collect::<Vec<&Variable>>();

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
        let child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name))
            .collect::<Vec<&Variable>>();

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
        let children: Vec<Variable> = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.parent_key == parent_key)
            .cloned()
            .collect::<Vec<Variable>>();

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
    /// Recursively process the deferred variables in the variable cache,
    /// and add their children to the cache.
    /// Enforce a max level, so that we don't recurse infinitely on circular references.
    #[allow(clippy::too_many_arguments)]
    pub fn recurse_deferred_variables(
        &mut self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        parent_variable: Option<&mut Variable>,
        max_recursion_depth: usize,
        current_recursion_depth: usize,
        frame_info: StackFrameInfo<'_>,
    ) {
        if current_recursion_depth >= max_recursion_depth {
            return;
        }
        let mut variable_to_recurse = if let Some(parent_variable) = parent_variable {
            parent_variable
        } else if let Some(parent_variable) = self.root_variable_mut() {
            // This is the top-level variable, which has no parent.
            parent_variable
        } else {
            // If the variable cache is empty, we have nothing to do.
            return;
        }
        .clone();

        if debug_info
            .cache_deferred_variables(self, memory, &mut variable_to_recurse, frame_info)
            .is_err()
        {
            return;
        };
        for mut child in self.get_children(variable_to_recurse.variable_key).unwrap() {
            self.recurse_deferred_variables(
                debug_info,
                memory,
                Some(&mut child),
                max_recursion_depth,
                current_recursion_depth + 1,
                frame_info,
            );
        }
    }

    /// Traverse the `VariableCache` and return a Vec of all the memory ranges that are referenced by the variables.
    /// This is used to determine which memory ranges to read from the target when creating a 'default' [`crate::CoreDump`].
    pub fn get_discrete_memory_ranges(&self) -> Vec<Range<u64>> {
        let mut memory_ranges: Vec<Range<u64>> = Vec::new();
        for variable in self.variable_hash_map.values() {
            if let Some(mut memory_range) = variable.memory_range() {
                // This memory may need to be read by 32-bit aligned words, so make sure
                // the range is aligned to 32 bits.
                memory_range.align_to_32_bits();
                if !memory_ranges.contains(&memory_range) {
                    memory_ranges.push(memory_range);
                }
            }
            // The datatype &str is a special case, because it is stores a pointer to the string data,
            // and the length of the string.
            if variable.type_name == VariableType::Struct("&str".to_string()) {
                if let Ok(children) = self.get_children(variable.variable_key) {
                    if !children.is_empty() {
                        let string_length = match children.iter().find(|child_variable| {
                            child_variable.name == VariableName::Named("length".to_string())
                        }) {
                            Some(string_length) => {
                                if string_length.is_valid() {
                                    string_length.get_value(self).parse().unwrap_or(0_usize)
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
                                    self.get_children(location_value.variable_key)
                                {
                                    if let Some(first_child) = child_variables.first() {
                                        first_child
                                            .memory_location
                                            .memory_address()
                                            .unwrap_or(0_u64)
                                    } else {
                                        0_u64
                                    }
                                } else {
                                    0_u64
                                }
                            }
                            None => 0_u64,
                        };
                        if string_location == 0 || string_length == 0 {
                            // We don't have enough information to read the string from memory.
                            // I've never seen an instance of this, but it is theoretically possible.
                            tracing::warn!(
                                "Failed to find string location or length for variable: {:?}",
                                variable
                            );
                        } else {
                            let mut memory_range =
                                string_location..(string_location + string_length as u64);
                            // This memory might need to be read by 32-bit aligned words, so make sure
                            // the range is aligned to 32 bits.
                            memory_range.align_to_32_bits();
                            if !memory_ranges.contains(&memory_range) {
                                memory_ranges.push(memory_range);
                            }
                        }
                    }
                };
            }
        }
        memory_ranges
    }
}

#[cfg(test)]
mod test {
    use gimli::{DebugInfoOffset, UnitOffset, UnitSectionOffset};
    use termtree::Tree;

    use crate::debug::{
        Variable, VariableCache, VariableLocation, VariableName, VariableNodeType, VariableType,
        VariantRole,
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
        let c = VariableCache::new_dwarf_cache(
            DebugInfoOffset(0).into(),
            UnitOffset(0),
            VariableName::StaticScopeRoot,
        );

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
        let mut cache = VariableCache::new_dwarf_cache(
            UnitSectionOffset::DebugInfoOffset(DebugInfoOffset(0)),
            UnitOffset(0),
            VariableName::Named("root".to_string()),
        );
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None, None).unwrap();

        let var_2 = cache.create_variable(root_key, None, None).unwrap();

        let children = cache.get_children(root_key).unwrap();

        let expected_children = vec![var_1, var_2];

        assert_eq!(children, expected_children);
    }

    #[test]
    fn find_entry() {
        let mut cache = VariableCache::new_dwarf_cache(
            UnitSectionOffset::DebugInfoOffset(DebugInfoOffset(0)),
            UnitOffset(0),
            VariableName::Named("root".to_string()),
        );
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None, None).unwrap();

        let _var_2 = cache.create_variable(root_key, None, None).unwrap();

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
        let mut cache = VariableCache::new_dwarf_cache(
            UnitSectionOffset::DebugInfoOffset(DebugInfoOffset(0)),
            UnitOffset(0),
            VariableName::Named("root".to_string()),
        );
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None, None).unwrap();

        let var_2 = cache.create_variable(root_key, None, None).unwrap();

        let var_3 = cache
            .create_variable(var_2.variable_key, None, None)
            .unwrap();

        let var_4 = cache
            .create_variable(var_2.variable_key, None, None)
            .unwrap();

        let var_5 = cache
            .create_variable(var_3.variable_key, None, None)
            .unwrap();

        let var_6 = cache.create_variable(root_key, None, None).unwrap();

        let var_7 = cache
            .create_variable(var_6.variable_key, None, None)
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
        let (mut cache, mut vars) = build_test_tree();
        let non_unique_name = VariableName::Named("non_unique_name".to_string());
        let unique_name = VariableName::Named("unique_name".to_string());

        show_tree(&cache);

        vars[3].name = non_unique_name.clone();
        cache.update_variable(&vars[3]).unwrap();

        show_tree(&cache);

        vars[4].name = unique_name.clone();
        cache.update_variable(&vars[4]).unwrap();

        show_tree(&cache);

        vars[6].name = non_unique_name.clone();
        cache.update_variable(&vars[6]).unwrap();

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
