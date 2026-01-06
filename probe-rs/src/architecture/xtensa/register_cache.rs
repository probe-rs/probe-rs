use std::collections::HashMap;

use crate::architecture::xtensa::arch::Register;

#[derive(Default)]
pub struct RegisterCache {
    entries: HashMap<Register, CacheEntry>,
}

impl RegisterCache {
    pub fn new() -> Self {
        RegisterCache {
            entries: HashMap::new(),
        }
    }

    /// Stores a register value in the cache.
    pub fn store(&mut self, id: Register, value: u32) {
        self.entries.insert(
            id,
            CacheEntry {
                original_value: value,
                current_value: value,
                dirty: false,
            },
        );
    }

    /// Iterates over all entries in the cache.
    pub fn iter(&self) -> impl Iterator<Item = (Register, &CacheEntry)> {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    pub(crate) fn mark_dirty(&mut self, register: Register) {
        let entry = self
            .entries
            .get_mut(&register)
            .unwrap_or_else(|| panic!("Register {register:?} is not in cache"));

        entry.dirty = true;
    }

    pub(crate) fn current_value_of(&self, register: Register) -> Option<u32> {
        self.entries.get(&register).map(|entry| entry.current_value)
    }

    pub(crate) fn original_value_of(&self, register: Register) -> Option<u32> {
        self.entries
            .get(&register)
            .map(|entry| entry.original_value)
    }

    pub(crate) fn remove(&mut self, register: Register) {
        self.entries.remove(&register);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct CacheEntry {
    /// The original value of the register, as loaded from the target.
    original_value: u32,

    /// The current value of the register in the target's register.
    ///
    /// This may be different from the original value if the register is dirty.
    current_value: u32,

    /// Indicates whether the register is dirty.
    dirty: bool,
}

impl CacheEntry {
    /// Returns whether the register is dirty, meaning the target's register value has been modified
    /// but not yet committed.
    pub fn is_dirty(&self) -> bool {
        self.original_value != self.current_value || self.dirty
    }
}
