/// All the shared options that control the behaviour of the debugger.
pub(crate) mod configuration;
/// The data structures borrowed from the [`SessionData`], that applies to a specific core.
pub(crate) mod core_data;
/// This is where the primary processing for the debugger is driven from.
pub(crate) mod debug_entry;
/// The debugger support for rtt.
pub(crate) mod debug_rtt;
/// The data structures needed to keep track of a [`SessionData`].
pub(crate) mod session_data;
