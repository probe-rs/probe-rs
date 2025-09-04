//! Types for tracking server-to-client JSON-RPC requests.

use std::fmt::{self, Debug, Formatter};
use std::future::Future;

use dashmap::{DashMap, mapref::entry::Entry};
use futures::channel::oneshot;
use tracing::warn;

use crate::cmd::dap_server::protocol::response::ResponseKind;

/// A hashmap containing pending client requests, keyed by request ID.
pub struct Pending(DashMap<i64, Vec<oneshot::Sender<ResponseKind>>>);

impl Pending {
    /// Creates a new pending client requests map.
    pub fn new() -> Self {
        Pending(DashMap::new())
    }

    /// Inserts the given response into the map.
    ///
    /// The corresponding `.wait()` future will then resolve to the given value.
    pub fn insert(&self, r: ResponseKind) {
        match self.0.entry(r.request_seq()) {
            Entry::Vacant(_) => warn!(
                "received response with unknown request ID: {}",
                r.request_seq()
            ),
            Entry::Occupied(mut entry) => {
                let tx = match entry.get().len() {
                    1 => entry.remove().remove(0),
                    _ => entry.get_mut().remove(0),
                };

                tx.send(r).expect("receiver already dropped");
            }
        }
    }

    /// Marks the given request ID as pending and waits for its corresponding response to arrive.
    ///
    /// If the same request ID is being waited upon in multiple locations, then the incoming
    /// response will be routed to one of the callers in a first come, first served basis. To
    /// ensure correct routing of JSON-RPC requests, each identifier value used _must_ be unique.
    pub fn wait(&self, id: i64) -> impl Future<Output = ResponseKind> + Send + 'static {
        let (tx, rx) = oneshot::channel();

        match self.0.entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(vec![tx]);
            }
            Entry::Occupied(mut entry) => {
                let txs = entry.get_mut();
                txs.reserve(1); // We assume concurrent waits are rare, so reserve one by one.
                txs.push(tx);
            }
        }

        async { rx.await.expect("sender already dropped") }
    }
}

impl Debug for Pending {
    fn fmt(&self, f: &mut Formatter) -> fmt::Result {
        #[derive(Debug)]
        struct Waiters(usize);

        let iter = self
            .0
            .iter()
            .map(|e| (e.key().clone(), Waiters(e.value().len())));

        f.debug_map().entries(iter).finish()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::cmd::dap_server::debug_adapter::dap::dap_types::Response;

    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn waits_for_client_response() {
        let pending = Pending::new();

        let id = 1;
        let wait_fut = pending.wait(id.clone());

        let response = ResponseKind::Ok(Response {
            body: Some(json!({})),
            command: "".into(),
            message: None,
            request_seq: id,
            seq: 0,
            success: true,
            type_: "response".into(),
        });
        pending.insert(response.clone());

        assert_eq!(wait_fut.await, response);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn routes_responses_in_fifo_order() {
        let pending = Pending::new();

        let id = 1;
        let wait_fut1 = pending.wait(id.clone());
        let wait_fut2 = pending.wait(id.clone());

        let foo = ResponseKind::Ok(Response {
            body: Some(json!("foo")),
            command: "".into(),
            message: None,
            request_seq: id.clone(),
            seq: 0,
            success: true,
            type_: "response".into(),
        });
        let bar = ResponseKind::Ok(Response {
            body: Some(json!("bar")),
            command: "".into(),
            message: None,
            request_seq: id.clone(),
            seq: 0,
            success: true,
            type_: "response".into(),
        });
        pending.insert(bar.clone());
        pending.insert(foo.clone());

        assert_eq!(wait_fut1.await, bar);
        assert_eq!(wait_fut2.await, foo);
    }
}
