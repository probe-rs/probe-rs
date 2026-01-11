use super::{
    dap_types::{EvaluateArguments, Response},
    repl_types::*,
};
use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{dap::adapter::DebugAdapter, protocol::ProtocolAdapter},
    server::core_data::CoreHandle,
};
use linkme::distributed_slice;
use std::{fmt::Display, time::Duration};

pub(crate) mod backtrace;
pub(crate) mod breakpoint;
pub(crate) mod cpu;
pub(crate) mod embedded_test;
pub(crate) mod info;
pub(crate) mod inspect;

/// The handler is a function that takes a reference to the target core, and a reference to the response body.
/// The response body is used to populate the response to the client.
/// The handler returns a Result<[`Response`], [`DebuggerError`]>.
///
/// We use the [`Response`] type here, so that we can have a consistent interface for processing the result as follows:
/// - The `command`, `success`, and `message` fields are the most commonly used fields for all the REPL commands.
/// - The `body` field is used if we need to pass back other DAP body types, e.g. [`BreakpointEventBody`].
/// - The remainder of the fields are unused/ignored.
///
/// The majority of the REPL command results will be populated into the response body.
//
// TODO: Make this less confusing by having a different struct for this.
pub(crate) type ReplHandler = fn(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> Result<Response, DebuggerError>;

#[derive(Clone, Copy)]
pub(crate) struct ReplCommand {
    /// The text that the user will type to invoke the command.
    /// - This is case sensitive.
    pub(crate) command: &'static str,
    pub(crate) help_text: &'static str,
    pub(crate) sub_commands: &'static [ReplCommand],
    pub(crate) args: &'static [ReplCommandArgs],
    pub(crate) requires_target_halted: bool,
    pub(crate) handler: ReplHandler,
}

impl Display for ReplCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.command)?;
        if !self.sub_commands.is_empty() {
            write!(f, " <subcommand>")?;
        }
        for arg in self.args {
            write!(f, " {arg}")?;
        }
        write!(f, ": {}", self.help_text)?;
        if !self.sub_commands.is_empty() {
            write!(f, "\n  Subcommands:")?;
            for sub_command in self.sub_commands {
                write!(f, "\n  - {sub_command}")?;
            }
        }
        Ok(())
    }
}

#[distributed_slice]
pub(crate) static REPL_COMMANDS: [ReplCommand];

#[distributed_slice(REPL_COMMANDS)]
static HELP: ReplCommand = ReplCommand {
    command: "help",
    help_text: "Information about available commands and how to use them.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[],
    handler: |target_core, _, _, _| {
        let mut help_text =
            "Usage:\t- Use <Ctrl+Space> to get a list of available commands.".to_string();
        help_text.push_str("\n\t- Use <Up/DownArrows> to navigate through the command list.");
        help_text.push_str("\n\t- Use <Hab> to insert the currently selected command.");
        help_text.push_str("\n\t- Note: This implementation is a subset of gdb commands, and is intended to behave similarly.");
        help_text.push_str("\nAvailable commands:");
        for command in target_core.core_data.repl_commands.iter() {
            help_text.push_str(&format!("\n{command}"));
        }
        Ok(Response {
            command: "help".to_string(),
            success: true,
            message: Some(help_text),
            type_: "response".to_string(),
            request_seq: 0,
            seq: 0,
            body: None,
        })
    },
};

#[distributed_slice(REPL_COMMANDS)]
static QUIT: ReplCommand = ReplCommand {
    command: "quit",
    help_text: "Disconnect (and suspend) the target.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[],
    handler: |target_core, _, _, _| {
        target_core.core.halt(Duration::from_millis(500))?;
        Ok(Response {
            command: "terminate".to_string(),
            success: true,
            message: Some("Debug Session Terminated".to_string()),
            type_: "response".to_string(),
            request_seq: 0,
            seq: 0,
            body: None,
        })
    },
};
