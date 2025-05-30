/// Implements the logic for each of the MS DAP types (events, requests, etc.)
pub(crate) mod adapter;
/// Add descriptions to CoreStatus.
pub(crate) mod core_status;
/// The MS DAP api (from json spec), and extensions (custom), for communicating with the MS DAP client.
pub(crate) mod dap_types;
pub(crate) mod error;
/// Handle the various "gdb-like" commands that are sent to the debug adapter, from the Debug Console REPL window.
/// These commands are not part of the DAP protocol, but are implemented by the debug adapter to provide a
/// gdb-like experience to users who prefer that to the VS Code UX.
/// It doesn't make sense to implement all gdb commands, and this implementation will focus on the ones that
/// are most useful to users, and this list is expected to grow over time.
pub(crate) mod repl_commands;
/// Helper functions to validate and build command subsets from [`repl_commands`].
pub(crate) mod repl_commands_helpers;
/// Various enums and structs used by the [`repl_commands::ReplCommand`].
pub(crate) mod repl_types;
/// The logic for handling the various MS DAP requests, implemented so that it can be used by both the
/// [`adapter::DebugAdapter`] and the [`repl_commands::ReplCommand`].
pub(crate) mod request_helpers;
