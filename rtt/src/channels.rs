//! List of RTT channels.

use crate::RttChannel;
use std::collections::{btree_map, BTreeMap};
use std::mem;

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
    pub fn iter(&self) -> Iter<'_, T> {
        Iter(self.0.iter())
    }

    /// Gets and iterator over the channels on the list, sorted by number.
    pub fn drain(&mut self) -> Drain<T> {
        let map = mem::take(&mut self.0);

        Drain(map.into_iter())
    }
}

/// An iterator over RTT channels.
///
/// This struct is created by the [`Channels::iter`] method. See its documentation for more.
pub struct Iter<'a, T: RttChannel>(btree_map::Iter<'a, usize, T>);

impl<'a, T: RttChannel> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(_, v)| v)
    }
}

/// A draining iterator over RTT channels.
///
/// This struct is created by the [`Channels::drain`] method. See its documentation for more.
pub struct Drain<T: RttChannel>(btree_map::IntoIter<usize, T>);

impl<T: RttChannel> Iterator for Drain<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(_, v)| v)
    }
}
