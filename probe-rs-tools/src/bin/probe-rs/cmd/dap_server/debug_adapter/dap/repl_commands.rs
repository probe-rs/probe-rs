use super::{
    dap_types::EvaluateArguments, repl_commands_helpers::build_expanded_commands, repl_types::*,
};
use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::dap::{
        adapter::DebugAdapter,
        dap_types::{EvaluateResponseBody, TerminatedEventBody},
    },
    server::core_data::CoreData,
};
use crate::util::style::ReplCommandName;
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
    adapter: &'a mut DebugAdapter,
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
            adapter: &'a mut DebugAdapter,
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

impl ReplCommand {
    /// Formats the command for the help output, with optional ANSI styling.
    pub(crate) fn help(&self, colorize: bool) -> ReplCommandHelp<'_> {
        ReplCommandHelp {
            command: self,
            colorize,
        }
    }
}

impl Display for ReplCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.help(false).fmt(f)
    }
}

/// A [`ReplCommand`] rendered for the help output.
pub(crate) struct ReplCommandHelp<'a> {
    command: &'a ReplCommand,
    colorize: bool,
}

impl Display for ReplCommandHelp<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let command = self.command;
        write!(
            f,
            "{}",
            ReplCommandName::new(command.command).colorize(self.colorize)
        )?;
        if !command.sub_commands.is_empty() {
            write!(f, " <subcommand>")?;
        }
        for arg in command.args {
            write!(f, " {arg}")?;
        }
        write!(f, ": {}", command.help_text)?;
        if !command.sub_commands.is_empty() {
            write!(f, "\n  Subcommands:")?;
            for sub_command in command.sub_commands {
                write!(f, "\n  - {}", sub_command.help(self.colorize))?;
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
    help_text: "Information about available commands, or about a specific command.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("command")],
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
    command_arguments: &'a str,
    _: &'a EvaluateArguments,
    debug_adapter: &'a mut DebugAdapter,
) -> EvalResult {
    Ok(EvalResponse::Message(help_text(
        &core_data.repl_commands,
        command_arguments,
        debug_adapter.supports_ansi_styling,
    )?))
}

fn help_text(
    commands: &[ReplCommand],
    topic: &str,
    colorize: bool,
) -> Result<String, DebuggerError> {
    let topic = topic.trim();
    if topic.is_empty() {
        let mut help_text = "Usage:".to_string();
        help_text.push_str("\n  - Use <Ctrl+Space> to get a list of available commands.");
        help_text.push_str("\n  - Use <Up/Down arrows> to navigate through the command list.");
        help_text.push_str("\n  - Use <Hab> to insert the currently selected command.");
        help_text.push_str("\nAvailable commands:");
        for command in commands {
            help_text.push_str(&format!("\n{}", command.help(colorize)));
        }
        return Ok(help_text);
    }

    let (_root, last_piece, matches) = build_expanded_commands(commands, topic);
    let selected: Vec<&ReplCommand> =
        if let Some(exact) = matches.iter().find(|command| command.command == last_piece) {
            vec![exact]
        } else {
            matches.iter().collect()
        };
    if selected.is_empty() {
        return Err(DebuggerError::UserMessage(format!(
            "Unknown command: {topic}."
        )));
    }

    Ok(selected
        .iter()
        .map(|command| command.help(colorize).to_string())
        .collect::<Vec<_>>()
        .join("\n"))
}

async fn need_subcommand<'a>(
    _backend: &'a mut RpcBackend,
    _core_data: &'a mut CoreData,
    _: &'a str,
    _: &'a EvaluateArguments,
    _: &'a mut DebugAdapter,
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
    adapter: &'a mut DebugAdapter,
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn help_output_styles_the_command_name_only_when_the_client_supports_ansi() {
        assert_eq!(
            HELP.help(false).to_string(),
            "help [command]: Information about available commands, or about a specific command."
        );
        assert_eq!(
            HELP.help(true).to_string(),
            "\u{1b}[1mhelp\u{1b}[0m [command]: Information about available commands, or about a specific command."
        );
    }

    #[test]
    fn help_topic_prints_one_command() {
        let text = help_text(&REPL_COMMANDS, "quit", false).unwrap();
        assert_eq!(text, QUIT.help(false).to_string());
        assert!(!text.contains("Available commands:"));
    }

    #[test]
    fn help_topic_prints_a_subcommand() {
        let text = help_text(&REPL_COMMANDS, "info locals", false).unwrap();
        assert!(text.contains("List local variables of the selected frame."));
        assert!(!text.contains("List all static variables."));
    }

    #[test]
    fn help_topic_rejects_unknown_commands() {
        let error = help_text(&REPL_COMMANDS, "not-a-command", false).unwrap_err();
        assert!(matches!(
            error,
            DebuggerError::UserMessage(message) if message.contains("not-a-command")
        ));
    }
}
