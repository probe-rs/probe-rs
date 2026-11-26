//! The write queues of a run loop.
//!
//! A write can wait for the target, for example a halted one, so it must not run in the dispatch
//! loop. While a run loop runs, its pollers run one worker for each stream and publish the stream
//! here. A write request that finds its stream puts the write in the queue and is answered at
//! once. The worker writes, and tries again until the target takes all of it.

use std::{
    collections::{HashMap, hash_map::Entry},
    hash::Hash,
    sync::Arc,
};

use probe_rs_rpc::{RpcError, RpcResult};
use tokio::sync::mpsc::{self, error::TrySendError};

/// The number of writes that can wait in one queue.
pub(crate) const QUEUE_DEPTH: usize = 8;

/// The streams of one kind that run loops publish, for example the RTT down channels.
pub(crate) struct Streams<K, S> {
    streams: Arc<parking_lot::Mutex<HashMap<K, Arc<S>>>>,
}

impl<K, S> Clone for Streams<K, S> {
    fn clone(&self) -> Self {
        Self {
            streams: self.streams.clone(),
        }
    }
}

impl<K: Eq + Hash + Copy, S> Streams<K, S> {
    pub fn new() -> Self {
        Self {
            streams: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    /// Publishes `stream` until the returned value drops. Returns `None` if `key` has a stream.
    pub fn publish(&self, key: K, stream: Arc<S>) -> Option<Published<K, S>> {
        let mut streams = self.streams.lock();
        let Entry::Vacant(entry) = streams.entry(key) else {
            return None;
        };
        entry.insert(stream);
        Some(Published {
            streams: self.clone(),
            key,
        })
    }

    pub fn get(&self, key: K) -> Option<Arc<S>> {
        self.streams.lock().get(&key).cloned()
    }
}

/// Removes a published stream when dropped.
pub(crate) struct Published<K: Eq + Hash, S> {
    streams: Streams<K, S>,
    key: K,
}

impl<K: Eq + Hash, S> Drop for Published<K, S> {
    fn drop(&mut self) {
        self.streams.streams.lock().remove(&self.key);
    }
}

/// Puts `write` in `queue`. Returns `len`, the number of bytes that the worker will write, or 0
/// if the queue is full, as for a full channel.
pub(crate) fn accept<T>(queue: &mpsc::Sender<T>, write: T, len: usize) -> RpcResult<u32> {
    match queue.try_send(write) {
        Ok(()) => Ok(len as u32),
        Err(TrySendError::Full(_)) => Ok(0),
        Err(TrySendError::Closed(_)) => Err(RpcError::from("The run loop stopped")),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::mpsc;

    use super::{QUEUE_DEPTH, Streams, accept};

    #[test]
    fn a_stream_is_published_until_its_guard_drops() {
        let streams = Streams::<u32, &str>::new();
        let published = streams.publish(1, Arc::new("first")).unwrap();
        assert!(streams.publish(1, Arc::new("second")).is_none());
        assert_eq!(streams.get(1).as_deref(), Some(&"first"));

        drop(published);
        assert!(streams.get(1).is_none());
    }

    #[test]
    fn a_full_queue_refuses_the_write() {
        let (queue, mut writes) = mpsc::channel(QUEUE_DEPTH);
        for write in 0..QUEUE_DEPTH {
            assert_eq!(accept(&queue, write, 3).unwrap(), 3);
        }
        assert_eq!(accept(&queue, QUEUE_DEPTH, 3).unwrap(), 0);

        // The writes keep their order.
        for write in 0..QUEUE_DEPTH {
            assert_eq!(writes.try_recv(), Ok(write));
        }
        drop(writes);
        assert!(accept(&queue, 0, 3).is_err());
    }
}
