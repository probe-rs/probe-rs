use std::{fmt::Write as _, ops::Range, path::Path, str::FromStr};

use linkme::distributed_slice;
use probe_rs::CoreDump;
use probe_rs_debug::{ObjectRef, VariableName};

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::{EvaluateArguments, MemoryAddress, Response},
            repl_commands::{REPL_COMMANDS, ReplCommand},
            repl_commands_helpers::{get_local_variable, memory_read},
            repl_types::{GdbFormat, GdbNuf, ReplCommandArgs},
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};

#[distributed_slice(REPL_COMMANDS)]
static PRINT: ReplCommand = ReplCommand {
    command: "p",
    // Stricly speaking, gdb refers to this as an expression, but we only support variables.
    help_text: "Print known information about variable.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[
        ReplCommandArgs::Optional("/f (f=format[n|v])"),
        ReplCommandArgs::Required("<local variable name>"),
    ],
    handler: print_variables,
};

#[distributed_slice(REPL_COMMANDS)]
static EXAMINE: ReplCommand = ReplCommand {
    command: "x",
    help_text: "Examine Memory, using format specifications, at the specified address.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[
        ReplCommandArgs::Optional("/Nuf (N=count, u=unit[b|h|w|g], f=format[t|x|i])"),
        ReplCommandArgs::Optional("address (hex)"),
    ],
    handler: examine_memory,
};

#[distributed_slice(REPL_COMMANDS)]
static DUMP: ReplCommand = ReplCommand {
    command: "dump",
    help_text: "Create a core dump at a target location. Specify memory ranges to dump, or leave blank to dump in-scope memory regions.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[
        ReplCommandArgs::Optional("memory start address"),
        ReplCommandArgs::Optional("memory size in bytes"),
        ReplCommandArgs::Optional("path (default: ./coredump)"),
    ],
    handler: dump_core,
};

fn print_variables(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> Result<Response, DebuggerError> {
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
                .check_supported_formats(&[GdbFormat::Native, GdbFormat::DapReference])
                .map_err(|error| DebuggerError::UserMessage(format!(
                    "Format specifier : {}, is not valid here.\nPlease select one of the supported formats:\n{error}", gdb_nuf.format_specifier,
                )))?;
        } else {
            variable_name = VariableName::Named(input_argument.to_string());
        }
    }

    get_local_variable(evaluate_arguments, target_core, variable_name, gdb_nuf)
}

fn examine_memory(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    request_arguments: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> Result<Response, DebuggerError> {
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
        if let Ok(MemoryAddress(addr)) = MemoryAddress::try_from(input_argument) {
            input_address = addr;
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
                "Invalid parameters. See the `help` command for more information.".to_string(),
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
}

fn dump_core(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> Result<Response, DebuggerError> {
    let mut args = command_arguments.split_whitespace().collect::<Vec<_>>();

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
                let &[start, size] = c else {
                    unreachable!("This should never be reached as there cannot be an odd number of arguments. Please report this as a bug.");
                };

                let start = parse_int::parse::<u64>(start)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?;
                let size = parse_int::parse::<u64>(size)
                    .map_err(|e| DebuggerError::UserMessage(e.to_string()))?;

                Ok::<_, DebuggerError>(start..start + size)
            })
            .collect::<Result<Vec<Range<u64>>, _>>()?
    };
    let mut range_string = String::new();
    for memory_range in &ranges {
        if !range_string.is_empty() {
            range_string.push_str(", ");
        }
        #[expect(clippy::unwrap_used, reason = "Writing to a string never fails")]
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
}
