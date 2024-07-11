use super::*;
use crate::{
    debug::{stack_frame::StackFrameInfo, unit_info::UnitInfo},
    Error,
};
use gimli::UnitOffset;
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
        struct VariableTreeNode<'c> {
            name: &'c VariableName,
            type_name: &'c VariableType,
            /// To eliminate noise, we will only show values for base data types and strings.
            value: String,
            /// ONLY If there are children.
            #[serde(skip_serializing_if = "Vec::is_empty")]
            children: Vec<VariableTreeNode<'c>>,
        }

        fn recurse_cache(variable_cache: &VariableCache) -> VariableTreeNode {
            let root_node = variable_cache.root_variable();

            VariableTreeNode {
                name: &root_node.name,
                type_name: &root_node.type_name,
                value: root_node.to_string(variable_cache),
                children: recurse_variables(variable_cache, root_node.variable_key, None),
            }
        }

        /// A helper function to recursively build the variable tree with `VariableTreeNode` entries.
        fn recurse_variables(
            variable_cache: &VariableCache,
            parent_variable_key: ObjectRef,
            max_children: Option<usize>,
        ) -> Vec<VariableTreeNode> {
            let mut children = variable_cache.get_children(parent_variable_key);

            let mut out = Vec::new();

            loop {
                if let Some(max_count) = max_children {
                    if out.len() >= max_count {
                        // Be a bit lenient with the limit, avoid showing "1 more" for a single child.
                        let remaining = children.clone().count();
                        if remaining > 1 {
                            break;
                        }
                    }
                }
                let Some(child_variable) = children.next() else {
                    break;
                };

                out.push(VariableTreeNode {
                    name: &child_variable.name,
                    type_name: &child_variable.type_name,
                    value: child_variable.to_string(variable_cache),
                    children: recurse_variables(
                        variable_cache,
                        child_variable.variable_key,
                        // Limit arrays to 50(+1) elements
                        child_variable.type_name.inner().is_array().then_some(50),
                    ),
                });
            }

            let remaining = children.count();
            if remaining > 0 {
                out.push(VariableTreeNode {
                    name: &VariableName::Artifical,
                    type_name: &VariableType::Unknown,
                    value: format!("... and {} more", remaining),
                    children: Vec::new(),
                });
            }

            out
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
    /// The `entries_offset` and `unit_info` values are used to
    /// extract the variable information from the debug information.
    ///
    /// The entries form a tree, only entries below the entry
    /// at `entries_offset` are considered when filling the cache.
    pub fn new_dwarf_cache(
        entries_offset: UnitOffset,
        name: VariableName,
        unit_info: &UnitInfo,
    ) -> Result<Self, DebugError> {
        let mut static_root_variable = Variable::new(Some(unit_info));
        static_root_variable.variable_node_type =
            VariableNodeType::DirectLookup(unit_info.debug_info_offset()?, entries_offset);
        static_root_variable.name = name;

        Ok(VariableCache::new(static_root_variable))
    }

    /// Create a cache for static variables.
    ///
    /// This will be filled with static variables when `cache_deferred_variables` is called.
    pub fn new_static_cache() -> Self {
        let mut static_root_variable = Variable::new(None);
        static_root_variable.variable_node_type = VariableNodeType::UnitsLookup;
        static_root_variable.name = VariableName::StaticScopeRoot;

        VariableCache::new(static_root_variable)
    }

    /// Get the root variable of the cache
    pub fn root_variable(&self) -> &Variable {
        &self.variable_hash_map[&self.root_variable_key]
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
        unit_info: Option<&UnitInfo>,
    ) -> Result<Variable, DebugError> {
        // Validate that the parent_key exists ...
        if !self.variable_hash_map.contains_key(&parent_key) {
            return Err(DebugError::Other(
                format!("VariableCache: Attempted to add a new variable with non existent `parent_key`: {:?}. Please report this as a bug", parent_key)
            ));
        }

        let mut variable_to_add = Variable::new(unit_info);
        variable_to_add.parent_key = parent_key;

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
                return Err(DebugError::Other(format!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", variable_to_add.name, variable_to_add.variable_key)));
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
    ) -> Result<(), DebugError> {
        // Validate that the parent_key exists ...
        if !self.variable_hash_map.contains_key(&parent_key) {
            return Err(DebugError::Other(format!("VariableCache: Attempted to add a new variable: {} with non existent `parent_key`: {:?}. Please report this as a bug", cache_variable.name, parent_key)));
        }

        cache_variable.parent_key = parent_key;

        if cache_variable.variable_key != ObjectRef::Invalid {
            return Err(DebugError::Other(format!("VariableCache: Attempted to add a new variable: {} with already set key: {:?}. Please report this as a bug", cache_variable.name, cache_variable.variable_key)));
        }

        // The caller is telling us this is definitely a new `Variable`
        cache_variable.variable_key = get_object_reference();

        tracing::trace!(
            "VariableCache: Add Variable: key={:?}, parent={:?}, name={:?}",
            cache_variable.variable_key,
            cache_variable.parent_key,
            cache_variable.name
        );

        if let Some(old_variable) = self
            .variable_hash_map
            .insert(cache_variable.variable_key, cache_variable.clone())
        {
            return Err(DebugError::Other(format!("Attempt to insert a new `Variable`:{:?} with a duplicate cache key: {:?}. Please report this as a bug.", cache_variable.name, old_variable.variable_key)));
        }

        Ok(())
    }

    /// Update a variable in the cache
    ///
    /// This function does not update the value of the variable.
    pub fn update_variable(&mut self, cache_variable: &Variable) -> Result<(), DebugError> {
        // Attempt to update an existing `Variable` in the cache
        tracing::trace!(
            "VariableCache: Update Variable, key={:?}, name={:?}",
            cache_variable.variable_key,
            &cache_variable.name
        );

        let Some(prev_entry) = self.variable_hash_map.get_mut(&cache_variable.variable_key) else {
            return Err(DebugError::Other(format!("Attempt to update an existing `Variable`:{:?} with a non-existent cache key: {:?}. Please report this as a bug.", cache_variable.name, cache_variable.variable_key)));
        };

        if cache_variable != prev_entry {
            tracing::trace!("Updated:  {:?}", cache_variable);
            tracing::trace!("Previous: {:?}", prev_entry);
            *prev_entry = cache_variable.clone();
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
        let child_variables = self.variable_hash_map.values().filter(|child_variable| {
            &child_variable.name == variable_name && child_variable.parent_key == parent_key
        });

        // Clone the iterator. This is cheap and makes rewinding easier.
        let mut first_iter = child_variables.clone();
        let first = first_iter.next();
        let more = first_iter.next().is_some();

        if more {
            let (last_index, last) = child_variables.enumerate().last().unwrap();
            tracing::error!(
                "Found {} variables with parent_key={:?} and name={}. Please report this as a bug.",
                last_index + 1,
                parent_key,
                variable_name
            );
            Some(last.clone())
        } else {
            first.cloned()
        }
    }

    /// Retrieve a clone of a specific `Variable`, using the `name`.
    /// If there is more than one, it will be logged (tracing::warn!), and only the first will be returned.
    /// It is possible for a hierarchy of variables in a cache to have duplicate names under different parents.
    pub fn get_variable_by_name(&self, variable_name: &VariableName) -> Option<Variable> {
        let mut child_variables = self
            .variable_hash_map
            .values()
            .filter(|child_variable| child_variable.name.eq(variable_name));

        let first = child_variables.next();
        let more = child_variables.next().is_some();

        if more {
            tracing::warn!(
                "Found {} variables with name={}. Please report this as a bug.",
                self.variable_hash_map.len(),
                variable_name
            );
        }

        first.cloned()
    }

    /// Retrieve `clone`d version of all the children of a `Variable`.
    /// If `parent_key == None`, it will return all the top level variables (no parents) in this cache.
    pub fn get_children(&self, parent_key: ObjectRef) -> impl Iterator<Item = &Variable> + Clone {
        self.variable_hash_map
            .values()
            .filter(move |child_variable| child_variable.parent_key == parent_key)
    }

    /// Check if variable has children. If the variable doesn't exist, it will return false.
    pub fn has_children(&self, parent_variable: &Variable) -> bool {
        self.get_children(parent_variable.variable_key)
            .next()
            .is_some()
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
            return Err(Error::Other(format!("Failed to remove a `VariableCache` entry with key: {:?}. Please report this as a bug.", variable_key)));
        };
        Ok(())
    }
    /// Recursively process the deferred variables in the variable cache,
    /// and add their children to the cache.
    /// Enforce a max level, so that we don't recurse infinitely on circular references.
    pub fn recurse_deferred_variables(
        &mut self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        max_recursion_depth: usize,
        frame_info: StackFrameInfo<'_>,
    ) {
        let mut parent_variable = self.root_variable().clone();

        self.recurse_deferred_variables_internal(
            debug_info,
            memory,
            &mut parent_variable,
            max_recursion_depth,
            0,
            frame_info,
        )
    }

    fn recurse_deferred_variables_internal(
        &mut self,
        debug_info: &DebugInfo,
        memory: &mut dyn MemoryInterface,
        parent_variable: &mut Variable,
        max_recursion_depth: usize,
        current_recursion_depth: usize,
        frame_info: StackFrameInfo<'_>,
    ) {
        if current_recursion_depth >= max_recursion_depth {
            return;
        }

        if debug_info
            .cache_deferred_variables(self, memory, parent_variable, frame_info)
            .is_err()
        {
            return;
        };

        let children: Vec<_> = self
            .get_children(parent_variable.variable_key)
            .cloned()
            .collect();

        for mut child in children {
            self.recurse_deferred_variables_internal(
                debug_info,
                memory,
                &mut child,
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
            if matches!(variable.type_name, VariableType::Struct(ref name) if name == "&str") {
                let children: Vec<_> = self.get_children(variable.variable_key).collect();
                if !children.is_empty() {
                    let string_length = match children.iter().find(|child_variable| {
                        matches!(child_variable.name, VariableName::Named(ref name) if name == "length")
                    }) {
                        Some(string_length) => {
                            if string_length.is_valid() {
                                string_length.to_string(self).parse().unwrap_or(0_usize)
                            } else {
                                0_usize
                            }
                        }
                        None => 0_usize,
                    };
                    let string_location = match children.iter().find(|child_variable| {
                        matches!(child_variable.name, VariableName::Named(ref name ) if name == "data_ptr")
                    }) {
                        Some(location_value) => {
                            let mut child_variables =
                                self.get_children(location_value.variable_key);
                            if let Some(first_child) = child_variables.next() {
                                first_child
                                    .memory_location
                                    .memory_address()
                                    .unwrap_or(0_u64)
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
            }
        }
        memory_ranges
    }
}

#[cfg(test)]
mod test {
    use termtree::Tree;

    use crate::debug::{
        Variable, VariableCache, VariableLocation, VariableName, VariableNodeType, VariableType,
        VariantRole,
    };

    fn show_tree(cache: &VariableCache) {
        let tree = build_tree(cache, cache.root_variable());

        println!("{}", tree);
    }

    fn build_tree(cache: &VariableCache, variable: &Variable) -> Tree<String> {
        let mut entry = Tree::new(format!(
            "{:?}: name={:?}, type={:?}, value={:?}",
            variable.variable_key,
            variable.name,
            variable.type_name,
            variable.to_string(cache)
        ));

        let children = cache.get_children(variable.variable_key);

        for child in children {
            entry.push(build_tree(cache, child));
        }

        entry
    }

    #[test]
    fn static_cache() {
        let c = VariableCache::new_static_cache();

        let cache_variable = c.root_variable();

        println!("{:#?}", cache_variable);

        //assert_eq!(cache_variable.parent_key, None);
        assert_eq!(cache_variable.name, VariableName::StaticScopeRoot);
        assert_eq!(cache_variable.type_name, VariableType::Unknown);
        assert_eq!(
            cache_variable.variable_node_type,
            VariableNodeType::UnitsLookup
        );

        assert_eq!(cache_variable.to_string(&c), "<unknown>");

        assert_eq!(cache_variable.source_location, Default::default());
        assert_eq!(cache_variable.memory_location, VariableLocation::Unknown);
        assert_eq!(cache_variable.byte_size, None);
        assert_eq!(cache_variable.member_index, None);
        assert_eq!(cache_variable.role, VariantRole::NonVariant);
    }

    #[test]
    fn find_children() {
        let mut cache = VariableCache::new_static_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None).unwrap();

        let var_2 = cache.create_variable(root_key, None).unwrap();

        let children: Vec<_> = cache.get_children(root_key).collect();

        let expected_children = vec![&var_1, &var_2];

        assert_eq!(children, expected_children);
    }

    #[test]
    fn find_entry() {
        let mut cache = VariableCache::new_static_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None).unwrap();

        let _var_2 = cache.create_variable(root_key, None).unwrap();

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
        let mut cache = VariableCache::new_static_cache();
        let root_key = cache.root_variable().variable_key;

        let var_1 = cache.create_variable(root_key, None).unwrap();

        let var_2 = cache.create_variable(root_key, None).unwrap();

        let var_3 = cache.create_variable(var_2.variable_key, None).unwrap();

        let var_4 = cache.create_variable(var_2.variable_key, None).unwrap();

        let var_5 = cache.create_variable(var_3.variable_key, None).unwrap();

        let var_6 = cache.create_variable(root_key, None).unwrap();

        let var_7 = cache.create_variable(var_6.variable_key, None).unwrap();

        assert_eq!(cache.len(), 8);

        let variables = vec![
            cache.root_variable().clone(),
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

        let new_children: Vec<_> = cache.get_children(vars[2].variable_key).collect();

        vars[5].parent_key = vars[2].variable_key;

        assert_eq!(new_children, vec![&vars[4], &vars[5]]);
    }
}
