use super::{
    core_status::DapStatus,
    dap_types::{
        BreakpointEventBody, EvaluateArguments, InstructionBreakpoint, MemoryAddress, Response,
    },
    repl_commands_helpers::*,
    repl_types::*,
    request_helpers::set_instruction_breakpoint,
};
use crate::cmd::dap_server::{
    DebuggerError, debug_adapter::dap::dap_types::Breakpoint, server::core_data::CoreHandle,
};
use itertools::Itertools;
use probe_rs::{CoreDump, CoreInterface, CoreStatus, HaltReason, RegisterValue};
use probe_rs_debug::{ColumnType, ObjectRef, StackFrame, VariableName};
use std::{
    fmt::{Display, Write as _},
    ops::Range,
    path::Path,
    str::FromStr,
    time::Duration,
};

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
    target_core: &mut CoreHandle,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
) -> Result<Response, DebuggerError>;

pub(crate) struct ReplCommand<H: 'static> {
    /// The text that the user will type to invoke the command.
    /// - This is case sensitive.
    pub(crate) command: &'static str,
    pub(crate) help_text: &'static str,
    pub(crate) sub_commands: &'static [ReplCommand<H>],
    pub(crate) args: &'static [ReplCommandArgs],
    pub(crate) handler: H,
}

impl<H> Display for ReplCommand<H> {
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

pub(crate) static REPL_COMMANDS: &[ReplCommand<ReplHandler>] = &[
    ReplCommand {
        command: "help",
        help_text: "Information about available commands and how to use them.",
        sub_commands: &[],
        args: &[],
        handler: |_, _, _| {
            let mut help_text =
                "Usage:\t- Use <Ctrl+Space> to get a list of available commands.".to_string();
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
        sub_commands: &[],
        args: &[],
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
        sub_commands: &[],
        args: &[],
        handler: |target_core, _, _| {
            target_core.core.run()?;
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
        sub_commands: &[],
        args: &[ReplCommandArgs::Optional("*address")],
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
            }

            let mut input_arguments = command_arguments.split_whitespace();
            let Some(address_str) = input_arguments.next().and_then(|arg| arg.strip_prefix('*'))
            else {
                return Err(DebuggerError::UserMessage(format!(
                    "Invalid parameters {command_arguments:?}. See the `help` command for more information."
                )));
            };

            let result = set_instruction_breakpoint(
                InstructionBreakpoint {
                    instruction_reference: address_str.to_string(),
                    condition: None,
                    hit_condition: None,
                    offset: None,
                    mode: None,
                },
                target_core,
            );
            let mut response = Response {
                command: "setBreakpoints".to_string(),
                success: true,
                message: Some(result.message.clone().unwrap_or_else(|| {
                    format!("Unexpected error creating breakpoint at {address_str}.")
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
            Ok(response)
        },
    },
    ReplCommand {
        command: "bt",
        sub_commands: &[ReplCommand {
            command: "yaml",
            help_text: "Print all information about the backtrace of the current thread to a local file in YAML format.",
            sub_commands: &[],
            args: &[ReplCommandArgs::Required(
                "path (e.g. my_dir/backtrace.yaml)",
            )],
            handler: |target_core, command_arguments, _| {
                let args = command_arguments.split_whitespace().collect_vec();

                let write_to_file = args.first().map(Path::new);

                // Using the `insta` crate to serialize, because they add a couple of transformations to the yaml output,
                // presumeably to make it easier to read.
                // In our case, we want this backtrace format to be comparable to the unwind tests
                // in `probe-rs::debug::debuginfo`.
                // The reason for this is that these 'live' backtraces are used to create the 'master' snapshots,
                // which is used to compare against backtraces generated from coredumps.
                use insta::_macro_support as insta_yaml;
                let yaml_data = insta_yaml::serialize_value(
                    &target_core.core_data.stack_frames,
                    insta_yaml::SerializationFormat::Yaml,
                );

                let response_message = if let Some(location) = write_to_file {
                    std::fs::write(location, yaml_data)
                        .map_err(|e| DebuggerError::UserMessage(format!("{e:?}")))?;
                    format!("Stacktrace successfully stored at {location:?}.")
                } else {
                    yaml_data
                };
                Ok(Response {
                    command: "backtrace".to_string(),
                    success: true,
                    message: Some(response_message),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                })
            },
        }],
        help_text: "Print the backtrace of the current thread.",
        args: &[],
        handler: |target_core, _, _| {
            let mut response_message = String::new();

            for (i, frame) in target_core.core_data.stack_frames.iter().enumerate() {
                response_message.push_str(&format!(
                    "Frame #{}: {}\n",
                    i + 1,
                    ReplStackFrame(frame)
                ));
            }

            Ok(Response {
                command: "backtrace".to_string(),
                success: true,
                message: Some(response_message),
                type_: "response".to_string(),
                request_seq: 0,
                seq: 0,
                body: None,
            })
        },
    },
    ReplCommand {
        command: "info",
        help_text: "Information of specified program data.",
        sub_commands: &[
            ReplCommand {
                command: "frame",
                help_text: "Describe the current frame, or the frame at the specified (hex) address.",
                sub_commands: &[],
                args: &[ReplCommandArgs::Optional("address")],
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: &[],
                args: &[],
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
                command: "reg",
                help_text: "List registers in the selected frame.",
                sub_commands: &[],
                args: &[ReplCommandArgs::Optional("register name")],
                handler: |target_core, command_arguments, _| {
                    let register_name = command_arguments.trim();
                    let regs = target_core.core.registers().all_registers().filter(|reg| {
                        if register_name.is_empty() {
                            true
                        } else {
                            reg.name().eq_ignore_ascii_case(register_name)
                        }
                    });

                    let mut results = vec![];
                    for reg in regs {
                        let reg_value: RegisterValue = target_core.core.read_core_reg(reg.id())?;
                        results.push((format!("{reg}:"), reg_value.to_string()));
                    }

                    if results.is_empty() {
                        return Err(DebuggerError::UserMessage(format!(
                            "No registers found matching {register_name:?}. See the `help` command for more information."
                        )));
                    }

                    Ok(Response {
                        command: "registers".to_string(),
                        success: true,
                        message: Some(reg_table(&results, 80)),
                        type_: "response".to_string(),
                        request_seq: 0,
                        seq: 0,
                        body: None,
                    })
                },
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: &[],
                args: &[],
                // TODO: This is easy to implement ... just requires deciding how to format the output.
                handler: |_, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "break",
                help_text: "List all breakpoints.",
                sub_commands: &[],
                args: &[],
                handler: |target_core, _, _| {
                    let breakpoint_addrs = target_core
                        .core
                        .hw_breakpoints()?
                        .into_iter()
                        .enumerate()
                        .filter_map(|(idx, bpt)| bpt.map(|bpt| (idx, bpt)));

                    let mut response_message = String::new();
                    if breakpoint_addrs.clone().count() == 0 {
                        response_message.push_str("No breakpoints set.");
                    } else {
                        for (idx, bpt) in breakpoint_addrs {
                            writeln!(&mut response_message, "Breakpoint #{idx} @ {bpt:#010X}\n")
                                .unwrap();
                        }
                    }

                    Ok(Response {
                        command: "breakpoints".to_string(),
                        success: true,
                        message: Some(response_message),
                        type_: "response".to_string(),
                        request_seq: 0,
                        seq: 0,
                        body: None,
                    })
                },
            },
        ],
        args: &[],
        handler: |_, _, _| {
            Err(DebuggerError::UserMessage("Please provide one of the required subcommands. See the `help` command for more information.".to_string()))
        },
    },
    ReplCommand {
        command: "p",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Print known information about variable.",
        sub_commands: &[],
        args: &[
            ReplCommandArgs::Optional("/f (f=format[n|v])"),
            ReplCommandArgs::Required("<local variable name>"),
        ],
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
                    let Some(gdb_nuf_string) = input_argument.strip_prefix('/') else {
                        return Err(DebuggerError::UserMessage(
                            "The '/' specifier must be followed by a valid gdb 'f' format specifier."
                                .to_string(),
                        ));
                    };
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
                    variable_name = VariableName::Named(input_argument.to_string());
                }
            }

            get_local_variable(evaluate_arguments, target_core, variable_name, gdb_nuf)
        },
    },
    ReplCommand {
        command: "x",
        help_text: "Examine Memory, using format specifications, at the specified address.",
        sub_commands: &[],
        args: &[
            ReplCommandArgs::Optional("/Nuf (N=count, u=unit[b|h|w|g], f=format[t|x|i])"),
            ReplCommandArgs::Optional("address (hex)"),
        ],
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
                    let Some(gdb_nuf_string) = input_argument.strip_prefix('/') else {
                        return Err(DebuggerError::UserMessage(
                            "The '/' specifier must be followed by a valid gdb 'Nuf' format specifier."
                                .to_string(),
                        ));
                    };

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
                        "Invalid parameters. See the `help` command for more information."
                            .to_string(),
                    ));
                }
            }
            if input_address == 0 {
                // No address was specified, so we'll use the frame address, if available.

                let frame_id = request_arguments.frame_id.map(ObjectRef::from);

                input_address = if let Some(frame_pc) = frame_id
                    .and_then(|frame_id| {
                        target_core
                            .core_data
                            .stack_frames
                            .iter()
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
        help_text: "Create a core dump at a target location. Specify memory ranges to dump, or leave blank to dump in-scope memory regions.",
        sub_commands: &[],
        args: &[
            ReplCommandArgs::Optional("memory start address"),
            ReplCommandArgs::Optional("memory size in bytes"),
            ReplCommandArgs::Optional("path (default: ./coredump)"),
        ],
        handler: |target_core, command_arguments, _| {
            let mut args = command_arguments.split_whitespace().collect_vec();

            // If we get an odd number of arguments, treat all n * 2 args at the start as memory blocks
            // and the last argument as the path tho store the coredump at.
            let location = Path::new(
                if args.len() % 2 != 0 {
                    args.pop()
                } else {
                    None
                }
                .unwrap_or("./coredump"),
            );

            let ranges = if args.is_empty() {
                // No specific memory ranges were requested, so we will dump the
                // memory ranges we know are specifically referenced by the variables
                // in the current scope.
                target_core.get_memory_ranges()
            } else {
                args
                .chunks(2)
                .map(|c| {
                    let start = if let Some(start) = c.first() {
                        parse_int::parse::<u64>(start)
                            .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
                    } else {
                        unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
                    };

                    let size = if let Some(size) = c.get(1) {
                        parse_int::parse::<u64>(size)
                            .map_err(|e| DebuggerError::UserMessage(e.to_string()))?
                    } else {
                        unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.")
                    };

                    Ok::<_, DebuggerError>(Range {start,end: start + size})
                })
                .collect::<Result<Vec<Range<u64>>, _>>()?
            };
            let mut range_string = String::new();
            for memory_range in &ranges {
                if !range_string.is_empty() {
                    write!(&mut range_string, ", ").unwrap();
                }
                write!(&mut range_string, "{memory_range:#X?}").unwrap();
            }
            range_string = if range_string.is_empty() {
                "(No memory ranges specified)".to_string()
            } else {
                format!("(Includes memory ranges: {range_string})")
            };
            CoreDump::dump_core(&mut target_core.core, ranges)?.store(location)?;

            Ok(Response {
                command: "dump".to_string(),
                success: true,
                message: Some(format!(
                    "Core dump {range_string} successfully stored at {location:?}.",
                )),
                type_: "response".to_string(),
                request_seq: 0,
                seq: 0,
                body: None,
            })
        },
    },
    ReplCommand {
        command: "clear",
        help_text: "Clear a breakpoint",
        sub_commands: &[],
        args: &[ReplCommandArgs::Required("*address")],
        handler: |target_core, args, _| {
            let mut input_arguments = args.split_whitespace();
            let Some(input_argument) = input_arguments.next() else {
                return Err(DebuggerError::UserMessage(
                    "Missing breakpoint address to clear. See the `help` command for more information.".to_string()
                ));
            };

            let Some(address_str) = input_argument.strip_prefix('*') else {
                return Err(DebuggerError::UserMessage(format!(
                    "Invalid input argument {input_argument}. See the `help` command for more information."
                )));
            };
            let Ok(MemoryAddress(address)) = address_str.try_into() else {
                return Err(DebuggerError::UserMessage(format!(
                    "Invalid memory address {address_str}. See the `help` command for more information."
                )));
            };
            target_core.clear_breakpoint(address)?;

            let response = Response {
                command: "setBreakpoints".to_string(),
                success: true,
                message: Some("Breakpoint cleared".to_string()),
                type_: "response".to_string(),
                request_seq: 0,
                seq: 0,
                body: serde_json::to_value(BreakpointEventBody {
                    breakpoint: Breakpoint {
                        id: Some(address as i64),
                        column: None,
                        end_column: None,
                        end_line: None,
                        instruction_reference: None,
                        line: None,
                        message: None,
                        offset: None,
                        source: None,
                        verified: false,
                        reason: None,
                    },
                    reason: "removed".to_string(),
                })
                .ok(),
            };
            Ok(response)
        },
    },
    ReplCommand {
        command: "reset",
        help_text: "Reset the target",
        sub_commands: &[],
        args: &[],
        handler: |target_core, _, _| {
            let core_info = target_core
                .core
                .reset_and_halt(Duration::from_millis(500))?;

            Ok(Response {
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
            })
        },
    },
];

struct ReplStackFrame<'a>(&'a StackFrame);

impl Display for ReplStackFrame<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header info for the StackFrame
        write!(f, "{}", self.0.function_name)?;
        if let Some(si) = &self.0.source_location {
            write!(f, "\n\t{}", si.path.to_path().display())?;

            if let (Some(column), Some(line)) = (si.column, si.line) {
                match column {
                    ColumnType::Column(c) => write!(f, ":{line}:{c}")?,
                    ColumnType::LeftEdge => write!(f, ":{line}")?,
                }
            }
        }
        Ok(())
    }
}

fn reg_table(results: &[(String, String)], max_line_length: usize) -> String {
    let mut max_reg_name_width = 0;
    let mut max_value_width = 0;

    // Calculate the maximum width of the register names and values
    for (reg_name, reg_value) in results {
        max_reg_name_width = max_reg_name_width.max(reg_name.len());
        max_value_width = max_value_width.max(reg_value.len());
    }

    let entry_width = max_value_width + max_reg_name_width + 1; // +1 for the space between name and value

    let mut response_message = String::new();
    let mut line_length = 0;
    for (reg_name, reg_value) in results {
        // Check if adding the line would exceed the maximum line length
        if line_length + entry_width > max_line_length {
            // If it does, start a new line
            response_message.push('\n');
            line_length = 0;
        }

        // Add the line to the response message
        if line_length != 0 {
            response_message.push(' ');
        }

        // Format the line name and value
        write!(
            &mut response_message,
            "{reg_name:<max_reg_name_width$} {reg_value:>max_value_width$}"
        )
        .unwrap();

        line_length += entry_width + 1; // +1 for the space between entries
    }
    response_message
}

#[cfg(test)]
mod test {
    #[test]
    fn reg_table_output() {
        let results = vec![
            ("PC/R0:".to_string(), "0x00000000".to_string()),
            ("R1:".to_string(), "0x00000001".to_string()),
            ("R2:".to_string(), "0x00000002".to_string()),
            ("R3:".to_string(), "0x00000003".to_string()),
            ("R4:".to_string(), "0x00000004".to_string()),
            ("R5:".to_string(), "0x00000005".to_string()),
        ];

        pretty_assertions::assert_eq!(
            super::reg_table(&results, 20),
            "PC/R0: 0x00000000\nR1:    0x00000001\nR2:    0x00000002\nR3:    0x00000003\nR4:    0x00000004\nR5:    0x00000005"
        );
        pretty_assertions::assert_eq!(
            super::reg_table(&results, 40),
            "PC/R0: 0x00000000 R1:    0x00000001\nR2:    0x00000002 R3:    0x00000003\nR4:    0x00000004 R5:    0x00000005"
        );
        pretty_assertions::assert_eq!(
            super::reg_table(&results, 80),
            "PC/R0: 0x00000000 R1:    0x00000001 R2:    0x00000002 R3:    0x00000003\nR4:    0x00000004 R5:    0x00000005"
        );
    }
}
