/// Everything related to the MS DAP implementation.
pub(crate) mod dap;
/// Communication interfaces to connect the DAP client and probe-rs-debugger.
pub(crate) mod protocol;
/// Handle the various "gdb-like" commands that are sent to the debug adapter, from the Debug Console REPL window.
/// These commands are not part of the DAP protocol, but are implemented by the debug adapter to provide a
/// gdb-like experience to users who prefer that to the VS Code UX.
/// It doesn't make sense to implement all gdb commands, and this implementation will focus on the ones that
/// are most useful to users, and this list is expected to grow over time.
pub(crate) mod repl_commands;
