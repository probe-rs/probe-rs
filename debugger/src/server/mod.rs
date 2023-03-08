/// All the shared options that control the behaviour of the debugger.
pub(crate) mod configuration;
/// The data structures borrowed from the [`session_data::SessionData`], that applies to a specific core.
pub(crate) mod core_data;
/// The debugger support for rtt.
pub(crate) mod debug_rtt;
/// The implementation of the [`debugger::debugger::Debugger`] internals.
pub(crate) mod debugger;
/// The data structures needed to keep track of a [`session_data::SessionData`].
pub(crate) mod session_data;
/// This is where the primary processing for the debugger is driven from.
pub(crate) mod startup;
