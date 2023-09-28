use super::{
    core_status::DapStatus,
    dap_types::{
        BreakpointEventBody, EvaluateArguments, InstructionBreakpoint, MemoryAddress, Response,
    },
    repl_commands_helpers::*,
    repl_types::*,
    request_helpers::set_instruction_breakpoint,
};
use crate::cmd::dap_server::{server::core_data::CoreHandle, DebuggerError};
use probe_rs::{debug::VariableName, CoreStatus, HaltReason};
use std::{fmt::Display, path::Path, str::FromStr, time::Duration};

/// The handler is a function that takes a reference to the target core, and a reference to the response body.
/// The response body is used to populate the response to the client.
/// The handler returns a Result<[`Response`], [`DebuggerError`]>.
/// We use the [`Response`] type here, so that we can have a consistent interface for processing the result as follows:
/// - The `command`, `success`, annd `message` fields are the most commonly used fields for all the REPL commands.
/// - The `body` field is used if we need to pass back other DAP body types, e.g. [`BreakpointEventBody`].
/// - The remainder of the fields are unused/ignored.
/// The majority of the REPL command results will be populated into the response body.
pub(crate) type ReplHandler = fn(
    target_core: &mut CoreHandle,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
) -> Result<Response, DebuggerError>;

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
        handler: |_, _, _| {
            let mut help_text =
                "Usage:\t-Use <Ctrl+Space> to get a list of available commands.".to_string();
            help_text.push_str("\n\t- Use <Up/DownArrows> to navigate through the command list.");
            help_text.push_str("\n\t- Use <Hab> to insert the currently selected command.");
            help_text.push_str("\n\t- Note: This implementation is a subset of gdb commands, and is intended to behave similarly.");
            help_text.push_str("\nAvailable commands:");
            for command in REPL_COMMANDS {
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
    },
    ReplCommand {
        command: "quit",
        help_text: "Disconnect (and suspend) the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, _, _| {
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
    },
    ReplCommand {
        command: "c",
        help_text: "Continue running the program on the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, _, _| {
            target_core.core.run()?;
            // Changing the status below will result in the debugger automaticlly synching the client status.
            target_core.core_data.last_known_status = CoreStatus::Running;
            Ok(Response {
                command: "continue".to_string(),
                success: true,
                message: Some(CoreStatus::Running.short_long_status(None).1),
                type_: "response".to_string(),
                request_seq: 0,
                seq: 0,
                body: None,
            })
        },
    },
    ReplCommand {
        command: "break",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Sets a breakpoint specified location, or next instruction if unspecified.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Optional("*address")]),
        handler: |target_core, command_arguments, _| {
            if command_arguments.is_empty() {
                let core_info = target_core.core.halt(Duration::from_millis(500))?;
                return Ok(Response {
                    command: "pause".to_string(),
                    success: true,
                    message: Some(
                        CoreStatus::Halted(HaltReason::Request)
                            .short_long_status(Some(core_info.pc))
                            .1,
                    ),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                });
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
                        let mut response = Response {
                            command: "setBreakpoints".to_string(),
                            success: true,
                            message: Some(result.message.clone().unwrap_or_else(|| {
                                format!("Unexpected error creating breakpoint at {input_argument}.")
                            })),
                            type_: "response".to_string(),
                            request_seq: 0,
                            seq: 0,
                            body: None,
                        };
                        if result.verified {
                            // The caller will catch this event body and use it to synch the UI breakpoint list.
                            response.body = serde_json::to_value(BreakpointEventBody {
                                breakpoint: result,
                                reason: "new".to_string(),
                            })
                            .ok();
                        }
                        return Ok(response);
                    }
                }
            }
            Err(DebuggerError::UserMessage(
                format!("Invalid parameters {command_arguments:?}. See the `help` command for more information."),
            ))
        },
    },
    ReplCommand {
        command: "backtrace",
        sub_commands: None,
        help_text: "Print the backtrace of the current thread.",
        args: None,
        // TODO: This is easy to implement ... just requires deciding how to format the output.
        handler: |_, _, _| Err(DebuggerError::Unimplemented),
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
                handler: |_, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: None,
                args: None,
                handler: |target_core, _, evaluate_arguments| {
                    let gdb_nuf = GdbNuf {
                        format_specifier: GdbFormat::Native,
                        ..Default::default()
                    };
                    let variable_name = VariableName::LocalScopeRoot;
                    get_local_variable(evaluate_arguments, target_core, variable_name, gdb_nuf)
                },
            },
            ReplCommand {
                command: "all-reg",
                help_text: "List all registers of the selected frame.",
                sub_commands: None,
                args: None,
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: None,
                args: None,
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _| Err(DebuggerError::Unimplemented),
            },
        ]),
        args: None,
        handler: |_, _, _| {
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
        handler: |target_core, command_arguments, evaluate_arguments| {
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

            get_local_variable(evaluate_arguments, target_core, variable_name, gdb_nuf)
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
        handler: |target_core, command_arguments, request_arguments| {
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
                    MemoryAddress(input_address) = input_argument.try_into()?;
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
                        .read_core_reg(target_core.core.program_counter())?
                }
            }

            memory_read(input_address, gdb_nuf, target_core)
        },
    },
    ReplCommand {
        command: "dump",
        help_text: "Create a core dump at a target location",
        sub_commands: None,
        args: Some(&[
            ReplCommandArgs::Optional("path"),
            ReplCommandArgs::Optional("heap-start"),
            ReplCommandArgs::Optional("heap-size"),
            ReplCommandArgs::Optional("heap-start"),
            ReplCommandArgs::Optional("path"),
        ]),
        handler: |target_core, command_arguments, _request_arguments| {
            let mut input_arguments = command_arguments.split_whitespace();
            let location = if let Some(location) = input_arguments.next() {
                Path::new(location)
            } else {
                Path::new("./coredump")
            };

            let stack_start = if let Some(input) = input_arguments.next() {
                parse_int::parse::<u64>(input)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
            } else {
                return Err(DebuggerError::UserMessage(
                    "A stack start is required.".to_string(),
                ));
            };

            let stack_size = if let Some(input) = input_arguments.next() {
                parse_int::parse::<u64>(input)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
            } else {
                return Err(DebuggerError::UserMessage(
                    "A stack size is required.".to_string(),
                ));
            };

            let heap_start = if let Some(input) = input_arguments.next() {
                parse_int::parse::<u64>(input)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
            } else {
                return Err(DebuggerError::UserMessage(
                    "A heap start is required.".to_string(),
                ));
            };

            let heap_size = if let Some(input) = input_arguments.next() {
                parse_int::parse::<u64>(input)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
            } else {
                return Err(DebuggerError::UserMessage(
                    "A heap size is required.".to_string(),
                ));
            };

            target_core
                .core
                .dump(
                    stack_start..stack_start + stack_size,
                    heap_start..heap_start + heap_size,
                    vec![],
                )?
                .store(location)?;

            Ok(Response {
                command: "dump".to_string(),
                success: true,
                message: None,
                type_: "response".to_string(),
                request_seq: 0,
                seq: 0,
                body: None,
            })
        },
    },
];
