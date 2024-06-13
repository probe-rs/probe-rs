/// All the shared options that control the behaviour of the debugger.
pub(crate) mod configuration;
/// The data structures borrowed from the [`session_data::SessionData`], that applies to a specific core.
pub(crate) mod core_data;
/// The debugger support for rtt.
pub(crate) mod debug_rtt;
/// Implements the part of the debug server that processes incoming requests from the [`DebugAdapter`](crate::cmd::dap_server::debug_adapter::dap::adapter::DebugAdapter).
pub(crate) mod debugger;
/// Manage the logging/tracing associated with the debugger.
pub(crate) mod logger;
/// The data structures needed to keep track of a session status in the debugger.
pub(crate) mod session_data;
/// This is where the primary processing for the debugger is driven from.
pub(crate) mod startup;
