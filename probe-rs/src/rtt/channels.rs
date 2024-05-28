//! List of RTT channels.

use crate::rtt::RttChannel;
use std::collections::{
    btree_map::{IntoValues, Values},
    BTreeMap,
};

/// List of RTT channels.
#[derive(Debug)]
pub struct Channels<T: RttChannel>(pub(crate) BTreeMap<usize, T>);

impl<T: RttChannel> Channels<T> {
    /// Creates a new empty list of channels.
    pub fn new() -> Self {
        Self(BTreeMap::new())
    }

    /// Appends a channel to the list.
    pub fn push(&mut self, channel: T) {
        let i = self.0.len();
        self.0.insert(i, channel);
    }

    /// Returns the number of channels on the list.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if the list is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Returns a reference to the channel corresponding to the number.
    pub fn get(&mut self, number: usize) -> Option<&T> {
        self.0.get(&number)
    }

    /// Removes the channel corresponding to the number from the list and returns it.
    pub fn take(&mut self, number: usize) -> Option<T> {
        self.0.remove(&number)
    }

    /// Gets and iterator over the channels on the list, sorted by number.
    pub fn iter(&self) -> Values<'_, usize, T> {
        self.0.values()
    }
}

impl<T: RttChannel> IntoIterator for Channels<T> {
    type Item = T;
    type IntoIter = IntoValues<usize, T>;

    /// Consumes the channel list and returns an iterator over the channels.
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_values()
    }
}
