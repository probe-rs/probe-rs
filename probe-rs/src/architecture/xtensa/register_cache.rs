use std::collections::HashMap;

use crate::{
    architecture::xtensa::{
        arch::Register,
        communication_interface::{MaybeDeferredResultIndex, XtensaError},
        xdm::Xdm,
    },
    probe::queue::DeferredResultIndex,
};

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
                original_value: CachedValue::Value(value),
                current_value: Some(value),
                dirty: false,
            },
        );
    }

    /// Stores a register value in the cache.
    pub fn store_deferred(&mut self, id: Register, deferred: DeferredResultIndex) {
        self.entries.insert(
            id,
            CacheEntry {
                original_value: CachedValue::Deferred(deferred),
                current_value: None,
                dirty: false,
            },
        );
    }

    /// Iterates over all entries in the cache.
    pub fn iter(&self) -> impl Iterator<Item = (Register, &CacheEntry)> {
        self.entries.iter().map(|(k, v)| (*k, v))
    }

    pub(crate) fn mark_dirty(&mut self, register: impl Into<Register>) {
        let register = register.into();
        let entry = self.entries.get_mut(&register).unwrap_or_else(|| {
            panic!("Register {register:?} is not in cache. This is a bug, please report it.")
        });

        entry.dirty = true;
    }

    pub(crate) fn resolve(
        &mut self,
        result: MaybeDeferredResultIndex,
        xdm: &mut Xdm<'_>,
    ) -> Result<u32, XtensaError> {
        match result {
            MaybeDeferredResultIndex::Value(value) => Ok(value),
            MaybeDeferredResultIndex::Deferred(register) => {
                let result = if let Some(entry) = self.entries.get_mut(&register) {
                    entry.current_value(xdm)
                } else {
                    panic!(
                        "Register {register:?} is not in cache. This is a bug, please report it."
                    );
                };

                if result.is_err() {
                    self.entries.remove(&register);
                }

                result
            }
        }
    }

    pub(crate) fn original_value_of(
        &mut self,
        register: Register,
    ) -> Option<MaybeDeferredResultIndex> {
        if let Some(entry) = self.entries.get_mut(&register) {
            let value = match entry.current_value {
                Some(value) => MaybeDeferredResultIndex::Value(value),
                None => MaybeDeferredResultIndex::Deferred(register),
            };
            Some(value)
        } else {
            None
        }
    }

    pub(crate) fn resolved_original_value_of(
        &mut self,
        register: Register,
        xdm: &mut Xdm<'_>,
    ) -> Option<Result<u32, XtensaError>> {
        self.entries
            .get_mut(&register)
            .map(|entry| entry.current_value(xdm))
    }

    pub(crate) fn remove(&mut self, register: Register) {
        self.entries.remove(&register);
    }
}

#[derive(PartialEq, Eq)]
pub(crate) enum CachedValue {
    /// The result is already available.
    Value(u32),

    /// The result is deferred.
    Deferred(DeferredResultIndex),
}

#[derive(PartialEq, Eq)]
pub struct CacheEntry {
    /// The original value of the register, as loaded from the target.
    ///
    /// May be deferred, in which case it is lazily loaded.
    original_value: CachedValue,

    /// The current value of the register in the target's register.
    ///
    /// This may be different from the original value if the register is dirty.
    ///
    /// For deferred values, this is None until the value is loaded.
    current_value: Option<u32>,

    /// Indicates whether the register is dirty.
    dirty: bool,
}

impl CacheEntry {
    /// Returns whether the register is dirty, meaning the target's register value has been modified
    /// but not yet committed.
    pub fn is_dirty(&self) -> bool {
        if self.dirty {
            return true;
        }

        // If the value has been loaded, compare it with the original value
        if let Some(current) = self.current_value {
            return CachedValue::Value(current) != self.original_value;
        }

        self.current_value.is_some()
    }

    fn current_value(&mut self, xdm: &mut Xdm<'_>) -> Result<u32, XtensaError> {
        if let Some(value) = self.current_value {
            return Ok(value);
        }

        let original_value = self.original_value(xdm)?;
        self.current_value = Some(original_value);
        Ok(original_value)
    }

    fn original_value(&mut self, xdm: &mut Xdm<'_>) -> Result<u32, XtensaError> {
        let value = match std::mem::replace(&mut self.original_value, CachedValue::Value(0)) {
            CachedValue::Value(value) => value,
            CachedValue::Deferred(index) => xdm.read_deferred_result(index)?.into_u32(),
        };

        self.original_value = CachedValue::Value(value);

        Ok(value)
    }
}
