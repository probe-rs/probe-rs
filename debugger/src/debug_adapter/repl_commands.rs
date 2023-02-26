use super::{
    dap_adapter::DapStatus,
    dap_types::{
        CompletionItem, CompletionItemType, CompletionsArguments, EvaluateArguments,
        EvaluateResponseBody, VariablePresentationHint,
    },
};
use crate::{
    debugger::{core_data::CoreHandle, debug_entry::DebugSessionStatus},
    DebuggerError,
};
use probe_rs::{debug::VariableName, CoreStatus, HaltReason, MemoryInterface};
use std::{fmt::Display, time::Duration};

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

/// The handler is a function that takes a reference to the target core, and a reference to the response body.
/// The response body is used to populate the response to the client.
/// The handler can return a [`DebugSessionStatus`], which is used to determine if the debug session should continue, or if it should be terminated.
/// The handler can also return a [`DebuggerError`], which is used to populate the response to the client.
/// The majority of the REPL command results will be populated into the response body.
type H = fn(
    target_core: &mut CoreHandle,
    response: &mut EvaluateResponseBody,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
) -> Result<DebugSessionStatus, DebuggerError>;

pub(crate) struct ReplCommand<H: 'static> {
    /// The text that the user will type to invoke the command.
    /// - This is case sensitive.
    pub(crate) command: &'static str,
    pub(crate) help_text: &'static str,
    pub(crate) sub_commands: Option<&'static [ReplCommand<H>]>,
    pub(crate) args: Option<&'static [ReplCommandArgs]>,
    pub(crate) handler: H,
}

impl<H> Display for ReplCommand<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ", self.command)?;
        if self.sub_commands.is_some() {
            write!(f, "<subcommand> ")?;
        }
        if let Some(args) = self.args {
            for arg in args {
                write!(f, " {} ", arg)?;
            }
        }
        write!(f, ": {} ", self.help_text)?;
        if let Some(sub_commands) = self.sub_commands {
            write!(f, "\n  Subcommands:")?;
            for sub_command in sub_commands {
                write!(f, "\n  - {}", sub_command)?;
            }
        }
        Ok(())
    }
}

static REPL_COMMANDS: &[ReplCommand<H>] = &[
    ReplCommand {
        command: "help",
        help_text: "Information about available commands and how to use them.",
        sub_commands: None,
        args: None,
        handler: |_, response_body, _, _| {
            let mut help_text =
                "Usage:\t-Use <Ctrl+Space> to get a list of available commands.".to_string();
            help_text.push_str("\n\t- Use <Up/DownArrows> to navigate through the command list.");
            help_text.push_str("\n\t- Use <Hab> to insert the currently selected command.");
            help_text.push_str("\n\t- Note: This implementation is a subset of gdb commands, and is intended to behave similarly.");
            help_text.push_str("\nAvailable commands:");
            for command in REPL_COMMANDS {
                help_text.push_str(&format!("\n{}", command));
            }
            response_body.result = help_text;
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "quit",
        help_text: "Disconnect (and suspend) the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, response_body, _, _| {
            target_core.core.halt(Duration::from_millis(500))?;
            response_body.result = "Debug Session Terminated".to_string();
            Ok(DebugSessionStatus::Terminate)
        },
    },
    ReplCommand {
        command: "c",
        help_text: "Continue running the program on the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, response_body, _, _| {
            target_core.core.run()?;
            response_body.result = CoreStatus::Running.short_long_status(None).1;
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "break",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Breakpoints:",
        sub_commands: Some(&[
            ReplCommand {
                command: "",
                help_text: "Halt the target at the next instruction.",
                sub_commands: None,
                args: None,
                handler: |target_core, response_body, _, _| {
                    let core_info = target_core.core.halt(Duration::from_millis(500))?;
                    response_body.result = CoreStatus::Halted(HaltReason::Request)
                        .short_long_status(Some(core_info.pc))
                        .1;
                    Ok(DebugSessionStatus::Continue)
                },
            },
            ReplCommand {
                command: "*",
                help_text: "Sets a breakpoint specified (hex) address.",
                sub_commands: None,
                args: Some(&[ReplCommandArgs::Required("address")]),
                handler: |target_core, response_body, command_arguments, _| {
                    println!("Setting breakpoint at address: {}", command_arguments);
                    let core_info = target_core.core.halt(Duration::from_millis(500))?;
                    response_body.result = CoreStatus::Halted(HaltReason::Request)
                        .short_long_status(Some(core_info.pc))
                        .1;
                    Ok(DebugSessionStatus::Continue)
                },
            },
        ]),
        args: None,
        handler: |_, response_body, _, _| {
            response_body.result = "Please provide one of the required subcommands. See the `help` command for more information.".to_string();
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "backtrace",
        sub_commands: None,
        help_text: "Print the backtrace of the current thread.",
        args: None,
        handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
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
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "frame",
                help_text:
                    "Describe the current frame, or the frame at the specified (hex) address.",
                sub_commands: None,
                args: Some(&[ReplCommandArgs::Optional("address")]),
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: None,
                args: None,
                handler: |target_core, response_body, _, evaluate_arguments| {
                    // Make sure we have a valid StackFrame
                    if let Some(stack_frame) = match evaluate_arguments.frame_id {
                        Some(frame_id) => target_core
                            .core_data
                            .stack_frames
                            .iter_mut()
                            .find(|stack_frame| stack_frame.id == frame_id),
                        None => {
                            // Use the current frame_id
                            target_core.core_data.stack_frames.first_mut()
                        }
                    } {
                        if let Some(variable_cache) = stack_frame.local_variables.as_mut() {
                            if let Some(local_variable_root) =
                                variable_cache.get_variable_by_name(&VariableName::LocalScopeRoot)
                            {
                                response_body.memory_reference =
                                    Some(format!("{}", local_variable_root.memory_location));
                                response_body.result =
                                    "Local Variables : Click to expand".to_string();
                                response_body.type_ =
                                    Some(format!("{:?}", local_variable_root.type_name));
                                response_body.variables_reference =
                                    local_variable_root.variable_key;
                                response_body.presentation_hint = Some(VariablePresentationHint {
                                    attributes: None,
                                    kind: None,
                                    lazy: Some(false),
                                    visibility: None,
                                });
                            } else {
                                response_body.result = format!(
                                    "No local variables found for frame: {:?}.",
                                    stack_frame.function_name
                                );
                            }
                        } else {
                            response_body.result = format!(
                                "No variables available for frame: {:?}.",
                                stack_frame.function_name
                            );
                        }
                    } else {
                        response_body.result = "No frame selected.".to_string();
                    }
                    Ok(DebugSessionStatus::Continue)
                },
            },
            ReplCommand {
                command: "all-reg",
                help_text: "List all registers of the selected frame.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
        ]),
        args: None,
        handler: |_, response_body, _, _| {
            response_body.result = "Please provide one of the required subcommands. See the `help` command for more information.".to_string();
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "p",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Print known information about variable.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Required("<variable name>")]),
        handler: |target_core, response_body, variable_name, evaluate_arguments| {
            // Make sure we have a valid StackFrame
            if let Some(stack_frame) = match evaluate_arguments.frame_id {
                Some(frame_id) => target_core
                    .core_data
                    .stack_frames
                    .iter_mut()
                    .find(|stack_frame| stack_frame.id == frame_id),
                None => {
                    // Use the current frame_id
                    target_core.core_data.stack_frames.first_mut()
                }
            } {
                if let Some(variable_cache) = stack_frame.local_variables.as_mut() {
                    if let Some(variable) = variable_cache
                        .get_variable_by_name(&VariableName::Named(variable_name.to_string()))
                    {
                        response_body.memory_reference =
                            Some(format!("{}", variable.memory_location));
                        response_body.result = format!(
                            "{} : {} ",
                            variable.name,
                            variable.get_value(variable_cache)
                        );
                        response_body.type_ = Some(format!("{:?}", variable.type_name));
                        response_body.variables_reference = variable.variable_key;
                    } else {
                        response_body.result = format!(
                            "No variable named {:?} found for frame: {:?}.",
                            variable_name, stack_frame.function_name
                        );
                    }
                } else {
                    response_body.result = format!(
                        "No variables available for frame: {:?}.",
                        stack_frame.function_name
                    );
                }
            } else {
                response_body.result = "No frame selected.".to_string();
            }
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "x",
        help_text: "Examine Memory.",
        sub_commands: Some(&[
            ReplCommand {
                command: "",
                help_text: "Examine Memory at specified address.",
                sub_commands: None,
                args: Some(&[ReplCommandArgs::Optional("address (hex)")]),
                handler: |target_core, response_body, command_arguments, _| {
                    if command_arguments.starts_with("0x") || command_arguments.starts_with("0X") {
                        if let Ok(memory_reference) =
                            u32::from_str_radix(&command_arguments[2..], 16)
                        {
                            let mut memory_result = [0u8; 4];
                            match target_core
                                .core
                                .read(memory_reference.into(), &mut memory_result)
                            {
                                Ok(()) => {
                                    response_body.result =
                                        format!("{:#010x}", u32::from_le_bytes(memory_result))
                                }
                                Err(err) => {
                                    response_body.result = format!(
                                        "Cannot read memory at address {:#010x}: {:?}",
                                        memory_reference, err
                                    )
                                }
                            }
                        } else {
                            response_body.result = "Invalid address.".to_string();
                        }
                    } else {
                        response_body.result =
                            "Invalid address. Please specify as a hex address, starting with `0x`."
                                .to_string();
                    }
                    Ok(DebugSessionStatus::Continue)
                },
            },
            ReplCommand {
                command: "/",
                help_text: "Examine Memory, using format specifications, at the specified address.",
                sub_commands: None,
                args: Some(&[
                    ReplCommandArgs::Optional("N - count of units"),
                    ReplCommandArgs::Optional(
                        "u - unit size: b(ytes), h(alfwords), w(ords), g(iant words",
                    ),
                    ReplCommandArgs::Optional("f - format: i(nstruction), t(binary), x(hex)"),
                    ReplCommandArgs::Optional("address (hex)"),
                ]),
                handler: |target_core, response_body, command_arguments, _| {
                    println!("memory request: {}", command_arguments);
                    let core_info = target_core.core.halt(Duration::from_millis(500))?;
                    response_body.result = CoreStatus::Halted(HaltReason::Request)
                        .short_long_status(Some(core_info.pc))
                        .1;
                    Ok(DebugSessionStatus::Continue)
                },
            },
        ]),
        args: None,
        handler: |_, response_body, _, _| {
            response_body.result = "Please provide one of the required subcommands. See the `help` command for more information.".to_string();
            Ok(DebugSessionStatus::Continue)
        },
    },
];

/// Get a list of command matches, based on the given command piece.
/// The `command_piece` is a valid [`ReplCommand`], which can be either a command or a sub_command.
fn find_commands<'a>(
    repl_commands: Vec<&'a ReplCommand<H>>,
    command_piece: &'a str,
) -> Vec<&'a ReplCommand<H>> {
    repl_commands
        .into_iter()
        .filter(move |command| command.command.starts_with(command_piece))
        .collect::<Vec<&ReplCommand<H>>>()
}

/// Iteratively builds a list of command matches, based on the given filter.
/// If multiple levels of commands are involved, the ReplCommand::command will be concatenated.
pub(crate) fn build_expanded_commands(command_filter: &str) -> (String, Vec<&ReplCommand<H>>) {
    // Split the given text into a command, optional sub-command, and optional arguments.
    let command_pieces = command_filter.split_whitespace();

    // Always start building from the top-level commands.
    let mut repl_commands: Vec<&ReplCommand<H>> = REPL_COMMANDS.iter().collect();

    let mut command_root = "".to_string();
    for command_piece in command_pieces {
        // Find the matching commands.
        let matches = find_commands(repl_commands.clone(), command_piece);

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

        // If there is only one match, and we expect arguments, then we need to return the match, and its arguments
        // TODO:

        if matches.is_empty() {
            // If there are no matches, then we can keep the matches from the previous iteration (if there were any).
        } else {
            // If there are multiple matches, or there is only one match with no sub-commands, then we can use the matches.
            repl_commands = matches;
        }
        break;
    }
    (command_root, repl_commands)
}

/// Returns a list of completion items for the REPL, based on matches to the given filter.
pub(crate) fn command_completions(arguments: CompletionsArguments) -> Vec<CompletionItem> {
    let (command_root, command_list) = if arguments.text.is_empty() {
        // If the filter is empty, then we can return all commands.
        (
            arguments.text,
            REPL_COMMANDS.iter().collect::<Vec<&ReplCommand<H>>>(),
        )
    } else {
        // Iterate over the command pieces, and find the matching commands.
        let (command_root, command_list) = build_expanded_commands(&arguments.text);
        (format!("{} ", command_root), command_list)
    };
    command_list
        .iter()
        .map(|command| CompletionItem {
            // Add a space after the command, so that the user can start typing the next command.
            // This space will be trimmed if the user selects to evaluate the command as is.
            label: format!("{}{} ", command_root, command.command),
            text: None,
            sort_text: None,
            detail: Some(command.to_string()),
            type_: Some(CompletionItemType::Keyword),
            start: None,
            length: None, //Some(arguments.column),
            selection_start: None,
            selection_length: None,
        })
        .collect()
}
