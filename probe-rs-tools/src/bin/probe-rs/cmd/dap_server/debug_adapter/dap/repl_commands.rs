use super::{dap_types::EvaluateArguments, repl_types::*};
use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::{EvaluateResponseBody, TerminatedEventBody},
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreData,
};
use linkme::distributed_slice;
use std::fmt::Display;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub(crate) mod backtrace;
pub(crate) mod breakpoint;
pub(crate) mod cpu;
pub(crate) mod embedded_test;
pub(crate) mod info;
pub(crate) mod inspect;
pub(crate) mod registers;
pub(crate) mod rtt;

/// Returns a boxed future so handlers can `.await` backend round trips without
/// a `block_on` bridge, while still being stored as a plain `fn` pointer in
/// the static [`REPL_COMMANDS`] table — which is shared across all sessions,
/// hence `&mut RpcBackend` rather than a generic `B`.
//
// TODO: Make this less confusing by having a different struct for this.
pub(crate) type ReplHandler = for<'a> fn(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> Pin<Box<dyn Future<Output = EvalResult> + 'a>>;

/// Wrap an `async fn` with the [`ReplHandler`] argument list so it can be
/// stored as a `fn` pointer in [`REPL_COMMANDS`].
macro_rules! async_fn {
    ($handler:ident) => {{
        fn wrapper<'a>(
            backend: &'a mut RpcBackend,
            core_data: &'a mut CoreData,
            command_arguments: &'a str,
            evaluate_arguments: &'a EvaluateArguments,
            adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
        ) -> ::std::pin::Pin<::std::boxed::Box<dyn ::std::future::Future<Output = EvalResult> + 'a>>
        {
            ::std::boxed::Box::pin($handler(
                backend,
                core_data,
                command_arguments,
                evaluate_arguments,
                adapter,
            ))
        }
        wrapper
    }};
}
pub(crate) use async_fn;

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
    handler: async_fn!(print_help),
};

#[distributed_slice(REPL_COMMANDS)]
static QUIT: ReplCommand = ReplCommand {
    command: "quit",
    help_text: "Disconnect (and suspend) the target.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[],
    handler: async_fn!(quit_repl),
};

async fn print_help<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _: &'a str,
    _: &'a EvaluateArguments,
    _: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let mut help_text =
        "Usage:\t- Use <Ctrl+Space> to get a list of available commands.".to_string();
    help_text.push_str("\n\t- Use <Up/DownArrows> to navigate through the command list.");
    help_text.push_str("\n\t- Use <Hab> to insert the currently selected command.");
    help_text.push_str(
        "\n\t- Note: This implementation is a subset of gdb commands, and is intended to behave similarly.",
    );
    help_text.push_str("\nAvailable commands:");
    for command in core_data.repl_commands.iter() {
        help_text.push_str(&format!("\n{command}"));
    }
    Ok(EvalResponse::Message(help_text))
}

async fn need_subcommand<'a>(
    _backend: &'a mut RpcBackend,
    _core_data: &'a mut CoreData,
    _: &'a str,
    _: &'a EvaluateArguments,
    _: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    Err(DebuggerError::UserMessage(
        "Please provide one of the required subcommands. See the `help` command for more information."
            .to_string(),
    ))
}

/// Halt the target and emit the `terminated` event (REPL `quit`).
async fn quit_repl<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _: &'a str,
    _: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    backend
        .halt(core_data.core_index, Duration::from_millis(500))
        .await?;
    adapter.dyn_send_event(
        "terminated",
        serde_json::to_value(TerminatedEventBody { restart: None }).ok(),
    )?;
    Ok(EvalResponse::Message(
        "Debug Session Terminated".to_string(),
    ))
}

pub enum EvalResponse {
    /// Successful evaluation, the result is a string.
    Message(String),

    /// Successful evaluation, the result is a complete evaluation response.
    Body(EvaluateResponseBody),
}

pub type EvalResult = Result<EvalResponse, DebuggerError>;
