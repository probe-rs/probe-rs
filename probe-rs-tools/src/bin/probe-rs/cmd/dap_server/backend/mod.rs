//! Backend abstraction for the DAP server.
//!
//! The DAP server historically operated directly against a local
//! [`probe_rs::Session`]. To allow the same debugger implementation to drive a
//! target via the probe-rs RPC layer (over a network connection), the
//! session-level operations the debugger actually needs are captured here in
//! the [`DapBackend`] trait.
//!
//! Two implementations are provided:
//!
//! * [`probe_rs::Session`] (local, blanket impl below).
//! * [`rpc::RpcBackend`], which forwards every operation to a probe-rs RPC
//!   server through a [`crate::rpc::client::RpcClient`].

pub mod rpc;

use probe_rs::{Core, CoreType, Error, Session, Target};

/// Session-level operations used by the DAP server.
///
/// Anything the DAP server needs to do against a "whole target" (as opposed to
/// a single [`Core`]) goes through this trait. The DAP code is written against
/// `SessionData<B: DapBackend>` so it can run against either a local
/// [`Session`] or a remote RPC-backed session implementation.
#[allow(
    dead_code,
    reason = "halted_access/as_local_session_mut are used by upcoming RPC backend"
)]
pub trait DapBackend {
    /// Return the available cores on this target.
    fn list_cores(&self) -> Vec<(usize, CoreType)>;

    /// Return the target description.
    fn target(&self) -> &Target;

    /// Return a handle to the requested core.
    fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error>;
}

impl DapBackend for Session {
    fn list_cores(&self) -> Vec<(usize, CoreType)> {
        Session::list_cores(self)
    }

    fn target(&self) -> &Target {
        Session::target(self)
    }

    fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error> {
        Session::core(self, core_index)
    }
}
