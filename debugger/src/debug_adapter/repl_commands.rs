use super::dap_types::{CompletionItem, CompletionItemType, CompletionsArguments};
use std::fmt::Display;

pub(crate) enum ReplCommandArgs {
    Required(&'static str),
    Optional(&'static str),
}

impl Display for ReplCommandArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplCommandArgs::Required(arg_value) => {
                write!(f, "{}", arg_value)
            }
            ReplCommandArgs::Optional(arg_value) => {
                write!(f, "[{}]", arg_value)
            }
        }
    }
}

pub(crate) struct ReplCommand {
    pub(crate) command: &'static str,
    pub(crate) help_text: &'static str,
    pub(crate) sub_commands: Option<&'static [ReplCommand]>,
    pub(crate) args: Option<&'static [ReplCommandArgs]>,
}

impl Display for ReplCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} : {} ", self.help_text, self.command)?;
        if let Some(args) = self.args {
            for arg in args {
                write!(f, " {}", arg)?;
            }
        }
        Ok(())
    }
}

static REPL_COMMANDS: &[ReplCommand] = &[
    ReplCommand {
        command: "help",
        help_text: "Print help for a specific command, or a list of 'all' supported commands.",
        sub_commands: None,
        args: None,
    },
    ReplCommand {
        command: "quit",
        help_text: "Disconnect (and suspend) the debuggee.",
        sub_commands: None,
        args: None,
    },
    ReplCommand {
        command: "backtrace",
        sub_commands: None,
        help_text: "Print the backtrace of the current thread.",
        args: None,
    },
    ReplCommand {
        command: "info",
        help_text: "Information of specified program data.",
        sub_commands: Some(&[
            ReplCommand {
                command: "threads",
                help_text: "List all threads.",
                sub_commands: None,
                args: None,
            },
            ReplCommand {
                command: "frame",
                help_text: "Describe the selected frame.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: None,
                args: None,
            },
            ReplCommand {
                command: "all-reg",
                help_text: "List all registers of the selected frame.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
            },
        ]),
        args: None,
    },
    ReplCommand {
        command: "p",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Print known information about variable.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Required("<variable name>")]),
    },
];

/// Get a list of command matches, based on the given command piece.
/// The `command_piece` is a valid [`ReplCommand`], which can be either a command or a sub_command.
fn find_commands<'a>(
    repl_commands: Vec<&'a ReplCommand>,
    command_piece: &'a str,
) -> Vec<&'a ReplCommand> {
    repl_commands
        .into_iter()
        .filter(move |command| command.command.starts_with(command_piece))
        .collect::<Vec<&ReplCommand>>()
}

/// Iteratively builds a list of command matches, based on the given filter.
/// If multiple levels of commands are involved, the ReplCommand::command will be concatenated.
fn build_expanded_commands(command_filter: &str) -> (String, Vec<&ReplCommand>) {
    // Split the given text into a command, optional sub-command, and optional arguments.
    let command_pieces = command_filter.split_whitespace();

    // Always start building from the top-level commands.
    let mut repl_commands: Vec<&ReplCommand> = REPL_COMMANDS.iter().collect();

    let mut command_root = "".to_string();
    for command_piece in command_pieces {
        // Find the matching commands.
        let matches = find_commands(repl_commands, command_piece);

        // If there is only one match, and it has sub-commands, then we can continue iterating (implicit recursion with new sub-command).
        if matches.len() == 1 {
            if let Some(parent_command) = matches.first() {
                if let Some(sub_commands) = parent_command.sub_commands {
                    // Build up the full command as we iterate ...
                    if !command_root.is_empty() {
                        command_root.push(' ');
                    }
                    command_root.push_str(parent_command.command);
                    repl_commands = sub_commands.iter().collect();
                    continue;
                }
            }
        }

        // If there are no matches, multiple matches, or there is only one match with no sub-commands, then we can return the matches.
        repl_commands = matches;
        break;
    }
    (command_root, repl_commands)
}

/// Returns a list of completion items for the REPL, based on matches to the given filter.
pub(crate) fn command_completions(arguments: CompletionsArguments) -> Vec<CompletionItem> {
    // Iterate over the command pieces, and find the matching commands.
    let (command_root, command_list) = build_expanded_commands(&arguments.text);
    command_list
        .iter()
        .map(|command| CompletionItem {
            // Add a space after the command, so that the user can start typing the next command.
            // This space will be trimmed if the user selects to evaluate the command as is.
            label: format!("{} {} ", command_root, command.command),
            text: None,
            sort_text: None,
            detail: Some(command.to_string()),
            type_: Some(CompletionItemType::Keyword),
            start: None,
            length: Some(arguments.column),
            selection_start: None,
            selection_length: None,
        })
        .collect()
}
