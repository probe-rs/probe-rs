//! Backend for the DAP server.
//!
//! The DAP server drives the target exclusively through [`rpc::RpcBackend`],
//! which forwards every operation to a probe-rs RPC server via
//! [`probe_rs_rpc_client::RpcClient`] — including the in-process RPC server
//! used for local sessions, so the debugger never touches a
//! [`probe_rs::Session`] directly.

pub mod rpc;

use probe_rs::CoreStatus;

use probe_rs_rpc::rtt_config::DataFormat;

/// UI event produced by semihosting handling, to be replayed on the DAP
/// adapter (RTT window open, console/RTT output). Backend-agnostic mirror of
/// the RPC `WireSemihostingUiEvent`.
#[derive(Debug, Clone)]
pub enum SemihostingUiEvent {
    RttWindow {
        handle: u32,
        path: String,
        format: DataFormat,
    },
    LogToConsole(String),
    RttOutput {
        handle: u32,
        data: String,
    },
}

/// Result of [`rpc::RpcBackend::handle_semihosting`]: the post-handling core
/// status and the UI events the caller must replay on the DAP adapter.
#[derive(Debug, Clone)]
pub struct SemihostingHandleResult {
    pub status: CoreStatus,
    pub events: Vec<SemihostingUiEvent>,
}
