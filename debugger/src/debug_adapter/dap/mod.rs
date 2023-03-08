/// Implements the logic for each of the MS DAP types (events, requests, etc.)
pub(crate) mod adapter;
/// Add descriptions to CoreStatus.
pub(crate) mod core_status;
/// The MS DAP api (from json spec), and extensions (custom), for communicating with the MS DAP client.
pub(crate) mod dap_types;
