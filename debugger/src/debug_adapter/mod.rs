/// Implements the logic for each of the MS DAP types (events, requests, etc.)
pub(crate) mod dap_adapter;
/// The MS DAP api, and extensions, for communicating with the MS DAP client.
pub(crate) mod dap_types;
/// Communication interfaces to connect the DAP client and probe-rs-debugger.
pub(crate) mod protocol;
