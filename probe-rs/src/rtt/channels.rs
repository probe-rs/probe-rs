//! List of RTT channels.

use std::vec::IntoIter;

use crate::rtt::RttChannel;

/// List of RTT channels.
#[derive(Debug)]
pub struct Channels<T: RttChannel>(pub(crate) Vec<Option<T>>);

impl<T: RttChannel> Default for Channels<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: RttChannel> Channels<T> {
    /// Creates a new empty list of channels.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Appends a channel to the list.
    pub fn push(&mut self, channel: T) {
        self.0.push(Some(channel));
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
        self.0.get(number).and_then(|c| c.as_ref())
    }

    /// Removes the channel corresponding to the number from the list and returns it.
    pub fn take(&mut self, number: usize) -> Option<T> {
        self.0.get_mut(number).and_then(|c| c.take())
    }

    /// Gets and iterator over the channels on the list, sorted by number.
    pub fn iter(&self) -> impl Iterator<Item = &'_ T> + '_ {
        self.0.iter().filter_map(|c| c.as_ref())
    }
}

impl<T: RttChannel> IntoIterator for Channels<T> {
    type Item = T;
    type IntoIter = ChannelsIter<T>;

    /// Consumes the channel list and returns an iterator over the channels.
    fn into_iter(self) -> Self::IntoIter {
        ChannelsIter(self.0.into_iter())
    }
}

/// Iterator over the channels on the list.
pub struct ChannelsIter<T: RttChannel>(IntoIter<Option<T>>);

impl<T: RttChannel> Iterator for ChannelsIter<T> {
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().and_then(|c| c)
    }
}
