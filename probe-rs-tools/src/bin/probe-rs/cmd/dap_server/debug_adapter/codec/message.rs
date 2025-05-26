use serde::{Deserialize, Serialize};

use crate::cmd::dap_server::debug_adapter::dap::dap_types::{Request, Response};

/// An incoming or outgoing JSON-RPC message.
#[derive(Deserialize, Serialize)]
#[cfg_attr(test, derive(Debug, PartialEq))]
#[serde(untagged)]
pub(crate) enum Message {
    /// A response message.
    Response(Response),
    /// A request or notification message.
    Request(Request),
}
