//! List of RTT channels.

use crate::rtt::RttChannel;
use std::collections::BTreeMap;

/// List of RTT channels.
#[derive(Debug)]
pub struct Channels<T: RttChannel>(pub(crate) BTreeMap<usize, T>);

impl<T: RttChannel> Channels<T> {
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
    pub fn iter(&self) -> impl Iterator<Item = &'_ T> + '_ {
        self.0.iter().map(|(_, v)| v)
    }

    /// Consumes the channel list and returns an iterator over the channels.
    pub fn into_iter(self) -> impl Iterator<Item = T> {
        self.0.into_iter().map(|(_, v)| v)
    }
}
