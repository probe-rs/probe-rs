use super::{
    core_status::DapStatus,
    dap_types::{EvaluateArguments, EvaluateResponseBody, InstructionBreakpoint},
    repl_commands_helpers::*,
    repl_types::*,
    request_helpers::set_instruction_breakpoint,
};
use crate::{
    server::{core_data::CoreHandle, debugger::DebugSessionStatus},
    DebuggerError,
};
use probe_rs::{debug::VariableName, CoreStatus, HaltReason};
use std::{fmt::Display, str::FromStr, time::Duration};

/// The handler is a function that takes a reference to the target core, and a reference to the response body.
/// The response body is used to populate the response to the client.
/// The handler can return a [`DebugSessionStatus`], which is used to determine if the debug session should continue, or if it should be terminated.
/// The handler can also return a [`DebuggerError`], which is used to populate the response to the client.
/// The majority of the REPL command results will be populated into the response body.
pub(crate) type ReplHandler = fn(
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
                write!(f, " {arg} ")?;
            }
        }
        write!(f, ": {} ", self.help_text)?;
        if let Some(sub_commands) = self.sub_commands {
            write!(f, "\n  Subcommands:")?;
            for sub_command in sub_commands {
                write!(f, "\n  - {sub_command}")?;
            }
        }
        Ok(())
    }
}

pub(crate) static REPL_COMMANDS: &[ReplCommand<ReplHandler>] = &[
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
                help_text.push_str(&format!("\n{command}"));
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
            target_core.core_data.last_known_status = CoreStatus::Running;
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "break",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Sets a breakpoint specified location, or next instruction if unspecified.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Optional("*address")]),
        handler: |target_core, response_body, command_arguments, _| {
            if command_arguments.is_empty() {
                let core_info = target_core.core.halt(Duration::from_millis(500))?;
                response_body.result = CoreStatus::Halted(HaltReason::Request)
                    .short_long_status(Some(core_info.pc))
                    .1;
            } else {
                let mut input_arguments = command_arguments.split_whitespace();
                if let Some(input_argument) = input_arguments.next() {
                    if let Some(address_str) = &input_argument.strip_prefix('*') {
                        let result = set_instruction_breakpoint(
                            InstructionBreakpoint {
                                instruction_reference: address_str.to_string(),
                                condition: None,
                                hit_condition: None,
                                offset: None,
                            },
                            target_core,
                        );
                        response_body.result = result.message.unwrap_or_else(|| {
                            format!("Unexpected error creating breakpoint at {input_argument}.")
                        });
                        if result.verified {
                            // TODO: Currently this sets breakpoints without synching the VSCode UI. We can send a Dap `breakpoint` event.
                        }
                    } else {
                        return Err(DebuggerError::UserMessage(
                            "Invalid parameters. See the `help` command for more information."
                                .to_string(),
                        ));
                    };
                }
            }
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "backtrace",
        sub_commands: None,
        help_text: "Print the backtrace of the current thread.",
        args: None,
        // TODO: This is easy to implement ... just requires deciding how to format the output.
        handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
    },
    ReplCommand {
        command: "info",
        help_text: "Information of specified program data.",
        sub_commands: Some(&[
            ReplCommand {
                command: "frame",
                help_text:
                    "Describe the current frame, or the frame at the specified (hex) address.",
                sub_commands: None,
                args: Some(&[ReplCommandArgs::Optional("address")]),
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: None,
                args: None,
                handler: |target_core, response_body, _, evaluate_arguments| {
                    let gdb_nuf = GdbNuf {
                        format_specifier: GdbFormat::Native,
                        ..Default::default()
                    };
                    let variable_name = VariableName::LocalScopeRoot;
                    get_local_variable(
                        evaluate_arguments,
                        target_core,
                        variable_name,
                        gdb_nuf,
                        response_body,
                    )
                },
            },
            ReplCommand {
                command: "all-reg",
                help_text: "List all registers of the selected frame.",
                sub_commands: None,
                args: None,
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: None,
                args: None,
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
        ]),
        args: None,
        handler: |_, _, _, _| {
            Err(DebuggerError::UserMessage("Please provide one of the required subcommands. See the `help` command for more information.".to_string()))
        },
    },
    ReplCommand {
        command: "p",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Print known information about variable.",
        sub_commands: None,
        args: Some(&[
            ReplCommandArgs::Optional("/f (f=format[n|v])"),
            ReplCommandArgs::Required("<local variable name>"),
        ]),
        handler: |target_core, response_body, command_arguments, evaluate_arguments| {
            let input_arguments = command_arguments.split_whitespace();
            let mut gdb_nuf = GdbNuf {
                format_specifier: GdbFormat::Native,
                ..Default::default()
            };
            // If no variable name is provided, use the root of the local scope, and print all it's children.
            let mut variable_name = VariableName::LocalScopeRoot;

            for input_argument in input_arguments {
                if input_argument.starts_with('/') {
                    if let Some(gdb_nuf_string) = input_argument.strip_prefix('/') {
                        gdb_nuf = GdbNuf::from_str(gdb_nuf_string)?;
                        gdb_nuf
                            .check_supported_formats(&[
                                GdbFormat::Native,
                                GdbFormat::DapReference,
                            ])
                            .map_err(|error| {
                                DebuggerError::UserMessage(format!(
                                    "Format specifier : {}, is not valid here.\nPlease select one of the supported formats:\n{error}", gdb_nuf.format_specifier
                                ))
                            })?;
                    } else {
                        return Err(DebuggerError::UserMessage(
                            "The '/' specifier must be followed by a valid gdb 'f' format specifier."
                                .to_string(),
                        ));
                    }
                } else {
                    variable_name = VariableName::Named(input_argument.to_string());
                }
            }

            get_local_variable(
                evaluate_arguments,
                target_core,
                variable_name,
                gdb_nuf,
                response_body,
            )
        },
    },
    ReplCommand {
        command: "x",
        help_text: "Examine Memory, using format specifications, at the specified address.",
        sub_commands: None,
        args: Some(&[
            ReplCommandArgs::Optional("/Nuf (N=count, u=unit[b|h|w|g], f=format[t|x|i])"),
            ReplCommandArgs::Optional("address (hex)"),
        ]),
        handler: |target_core, response_body, command_arguments, request_arguments| {
            let input_arguments = command_arguments.split_whitespace();
            let mut gdb_nuf = GdbNuf {
                ..Default::default()
            };
            // Sequence of evaluations will be:
            // 1. Specified address
            // 2. Frame address
            // 3. Program counter
            let mut input_address = 0_u64;

            for input_argument in input_arguments {
                if input_argument.starts_with("0x") || input_argument.starts_with("0X") {
                    if let Ok(memory_reference) = u64::from_str_radix(&input_argument[2..], 16) {
                        input_address = memory_reference;
                    } else {
                        return Err(DebuggerError::UserMessage(
                            "Invalid hex address.".to_string(),
                        ));
                    }
                } else if input_argument.starts_with('/') {
                    if let Some(gdb_nuf_string) = input_argument.strip_prefix('/') {
                        gdb_nuf = GdbNuf::from_str(gdb_nuf_string)?;
                        gdb_nuf
                            .check_supported_formats(&[
                                GdbFormat::Binary,
                                GdbFormat::Hex,
                                GdbFormat::Instruction,
                            ])
                            .map_err(|error| {
                                DebuggerError::UserMessage(format!(
                                    "Format specifier : {}, is not valid here.\nPlease select one of the supported formats:\n{error}", gdb_nuf.format_specifier
                                ))
                            })?;
                    } else {
                        return Err(DebuggerError::UserMessage(
                            "The '/' specifier must be followed by a valid gdb 'Nuf' format specifier."
                                .to_string(),
                        ));
                    }
                } else {
                    return Err(DebuggerError::UserMessage(
                        "Invalid parameters. See the `help` command for more information."
                            .to_string(),
                    ));
                }
            }
            if input_address == 0 {
                // No address was specified, so we'll use the frame address, if available.
                input_address = if let Some(frame_pc) = request_arguments
                    .frame_id
                    .and_then(|frame_id| {
                        target_core
                            .core_data
                            .stack_frames
                            .iter_mut()
                            .find(|stack_frame| stack_frame.id == frame_id)
                    })
                    .map(|stack_frame| stack_frame.pc)
                {
                    frame_pc.try_into()?
                } else {
                    target_core
                        .core
                        .read_core_reg(target_core.core.registers().program_counter())?
                }
            }

            memory_read(input_address, gdb_nuf, target_core, response_body)
        },
    },
];
