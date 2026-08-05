use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use probe_rs::probe::DebugProbeSelector;
use tokio::sync::oneshot;

/// Grants exclusive use of a probe to one waiter at a time, in the order the
/// waiters arrived.
///
/// [`ProbeBroker::acquire`] carries no deadline: a caller that no longer wants
/// the probe drops the future, which takes it out of the queue.
pub struct ProbeBroker {
    inner: Mutex<Inner>,
}

struct Inner {
    lanes: HashMap<DebugProbeSelector, Lane>,
}

#[derive(Default)]
struct Lane {
    occupied: bool,
    waiters: VecDeque<Waiter>,
    next_ticket: u64,
}

struct Waiter {
    ticket: u64,
    grant: oneshot::Sender<()>,
}

/// Exclusive use of one probe. Dropping it passes the probe to the next waiter.
pub struct ProbeLease {
    broker: Arc<ProbeBroker>,
    selector: DebugProbeSelector,
}

impl Default for ProbeBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl ProbeBroker {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                lanes: HashMap::new(),
            }),
        }
    }

    /// Wait until the probe is free, then take it.
    ///
    /// Drop the future to leave the queue.
    pub async fn acquire(self: &Arc<Self>, selector: DebugProbeSelector) -> ProbeLease {
        let (ticket, granted) = {
            let mut inner = self.inner.lock();
            let lane = inner.lanes.entry(selector.clone()).or_default();

            let ticket = lane.next_ticket;
            lane.next_ticket += 1;

            if !lane.occupied && lane.waiters.is_empty() {
                lane.occupied = true;
                return self.lease(selector);
            }

            let (grant, granted) = oneshot::channel();
            lane.waiters.push_back(Waiter { ticket, grant });
            (ticket, granted)
        };

        // The guard leaves the queue if this future is dropped before the probe
        // is handed over.
        let mut guard = WaitGuard {
            broker: self,
            selector: selector.clone(),
            ticket,
            defused: false,
        };

        let _ = granted.await;
        guard.defused = true;

        self.lease(selector)
    }

    #[cfg(test)]
    fn queued(&self, selector: &DebugProbeSelector) -> usize {
        self.inner
            .lock()
            .lanes
            .get(selector)
            .map_or(0, |lane| lane.waiters.len())
    }

    fn lease(self: &Arc<Self>, selector: DebugProbeSelector) -> ProbeLease {
        ProbeLease {
            broker: Arc::clone(self),
            selector,
        }
    }

    /// Hand the probe to the next waiter, or mark it free if nobody wants it.
    fn release(&self, selector: &DebugProbeSelector) {
        let mut inner = self.inner.lock();
        let Some(lane) = inner.lanes.get_mut(selector) else {
            return;
        };

        while let Some(waiter) = lane.waiters.pop_front() {
            if waiter.grant.send(()).is_ok() {
                // The lane stays occupied: ownership moved to that waiter.
                return;
            }
        }

        lane.occupied = false;
        inner.lanes.remove(selector);
    }
}

/// Removes a waiter that gave up. If the waiter is no longer queued, the probe
/// was already handed to it, so the lane must be passed on instead.
struct WaitGuard<'a> {
    broker: &'a Arc<ProbeBroker>,
    selector: DebugProbeSelector,
    ticket: u64,
    defused: bool,
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        if self.defused {
            return;
        }

        let queued = {
            let mut inner = self.broker.inner.lock();
            let Some(lane) = inner.lanes.get_mut(&self.selector) else {
                return;
            };

            let queued = lane.waiters.iter().any(|w| w.ticket == self.ticket);
            lane.waiters.retain(|w| w.ticket != self.ticket);
            if queued && !lane.occupied && lane.waiters.is_empty() {
                inner.lanes.remove(&self.selector);
            }
            queued
        };

        if !queued {
            self.broker.release(&self.selector);
        }
    }
}

impl Drop for ProbeLease {
    fn drop(&mut self) {
        self.broker.release(&self.selector);
    }
}

impl fmt::Debug for ProbeBroker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ProbeBroker")
    }
}

impl fmt::Debug for ProbeLease {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProbeLease")
            .field("selector", &self.selector)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::timeout;

    use super::*;

    fn selector(serial: &str) -> DebugProbeSelector {
        DebugProbeSelector {
            vendor_id: 0x1234,
            product_id: 0x5678,
            serial_number: Some(serial.to_owned()),
            interface: None,
        }
    }

    #[tokio::test]
    async fn free_probe_is_granted_immediately() {
        let broker = Arc::new(ProbeBroker::new());
        let lease = timeout(Duration::ZERO, broker.acquire(selector("a"))).await;
        assert!(lease.is_ok());
    }

    #[tokio::test]
    async fn busy_probe_is_not_granted_immediately() {
        let broker = Arc::new(ProbeBroker::new());
        let _lease = broker.acquire(selector("a")).await;

        let denied = timeout(Duration::ZERO, broker.acquire(selector("a"))).await;
        assert!(denied.is_err());
    }

    #[tokio::test]
    async fn different_probes_do_not_block_each_other() {
        let broker = Arc::new(ProbeBroker::new());
        let _lease = broker.acquire(selector("a")).await;

        let other = timeout(Duration::ZERO, broker.acquire(selector("b"))).await;
        assert!(other.is_ok());
    }

    #[tokio::test]
    async fn release_on_drop_wakes_next_waiter() {
        let broker = Arc::new(ProbeBroker::new());
        let lease = broker.acquire(selector("a")).await;

        let waiter = tokio::spawn({
            let broker = Arc::clone(&broker);
            async move { broker.acquire(selector("a")).await }
        });

        tokio::task::yield_now().await;
        drop(lease);

        assert!(timeout(Duration::from_secs(5), waiter).await.is_ok());
    }

    #[tokio::test]
    async fn waiters_are_served_in_order() {
        let broker = Arc::new(ProbeBroker::new());
        let lease = broker.acquire(selector("a")).await;

        let (order_tx, mut order_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut waiters = Vec::new();
        for id in 0..3 {
            let broker = Arc::clone(&broker);
            let order_tx = order_tx.clone();
            waiters.push(tokio::spawn(async move {
                let lease = broker.acquire(selector("a")).await;
                order_tx.send(id).unwrap();
                lease
            }));
            // Give each waiter time to enqueue, so the order is well defined.
            tokio::task::yield_now().await;
        }
        drop(order_tx);

        drop(lease);
        for waiter in waiters {
            let lease = timeout(Duration::from_secs(5), waiter)
                .await
                .unwrap()
                .unwrap();
            drop(lease);
        }

        let mut order = Vec::new();
        while let Some(id) = order_rx.recv().await {
            order.push(id);
        }
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn dropping_a_waiter_removes_it_from_the_queue() {
        let broker = Arc::new(ProbeBroker::new());
        let lease = broker.acquire(selector("a")).await;

        // This waiter gives up before the probe is free.
        timeout(Duration::from_millis(10), broker.acquire(selector("a")))
            .await
            .unwrap_err();
        assert_eq!(broker.queued(&selector("a")), 0);

        let waiter = tokio::spawn({
            let broker = Arc::clone(&broker);
            async move { broker.acquire(selector("a")).await }
        });
        tokio::task::yield_now().await;
        drop(lease);

        assert!(timeout(Duration::from_secs(5), waiter).await.is_ok());
    }
}
