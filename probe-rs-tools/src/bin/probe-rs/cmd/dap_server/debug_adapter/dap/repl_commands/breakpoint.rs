use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};
use typed_path::TypedPath;

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            core_status::DapStatus,
            dap_types::{
                Breakpoint, BreakpointEventBody, EvaluateArguments, InstructionBreakpoint,
                MemoryAddress, Source,
            },
            repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand},
            repl_types::ReplCommandArgs,
            request_helpers::set_instruction_breakpoint,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};

#[distributed_slice(REPL_COMMANDS)]
static BREAK: ReplCommand = ReplCommand {
    command: "break",
    help_text: "Set a breakpoint at a location, or halt the target if unspecified.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("*address | file:line")],
    handler: create_breakpoint,
};

#[distributed_slice(REPL_COMMANDS)]
static CLEAR: ReplCommand = ReplCommand {
    command: "clear",
    help_text: "Clear a breakpoint.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Required("*address | file:line")],
    handler: clear_breakpoint,
};

enum BreakpointLocation<'a> {
    Address(u64),
    FileLine { path: &'a str, line: u64 },
}

/// Parse `*<address>` or `<file>:<line>` from a single REPL argument token.
fn parse_breakpoint_location(input: &str) -> Result<BreakpointLocation<'_>, DebuggerError> {
    if let Some(addr_str) = input.strip_prefix('*') {
        let MemoryAddress(address) = addr_str.try_into()?;
        return Ok(BreakpointLocation::Address(address));
    }

    // Use rsplit so Windows drive letters (e.g. `C:\foo.rs:42`) are handled correctly.
    if let Some((path, line_str)) = input.rsplit_once(':')
        && let Ok(line) = line_str.parse::<u64>()
    {
        return Ok(BreakpointLocation::FileLine { path, line });
    }

    Err(DebuggerError::UserMessage(format!(
        "Invalid argument {input:?}. Expected `*<address>` or `<file>:<line>`. See `help`."
    )))
}

fn create_breakpoint(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    if command_arguments.is_empty() {
        let core_info = adapter.pause_impl(target_core)?;
        return Ok(EvalResponse::Message(
            CoreStatus::Halted(HaltReason::Request)
                .short_long_status(Some(core_info.pc))
                .1,
        ));
    }

    let Some(token) = command_arguments.split_whitespace().next() else {
        return Err(DebuggerError::UserMessage(
            "Missing argument. See `help`.".to_string(),
        ));
    };

    match parse_breakpoint_location(token)? {
        BreakpointLocation::Address(address) => {
            let result = set_instruction_breakpoint(
                InstructionBreakpoint {
                    instruction_reference: format!("{address:#x}"),
                    condition: None,
                    hit_condition: None,
                    offset: None,
                    mode: None,
                },
                target_core,
            );

            let body = if result.verified {
                serde_json::to_value(BreakpointEventBody {
                    breakpoint: result.clone(),
                    reason: "new".to_string(),
                })
                .ok()
            } else {
                None
            };

            adapter.dyn_send_event("breakpoint", body)?;

            Ok(EvalResponse::Message(result.message.unwrap_or_else(|| {
                format!("Unexpected error creating breakpoint at {address:#x}.")
            })))
        }

        BreakpointLocation::FileLine { path, line } => {
            let source = source_from_path(path);
            let verified = target_core
                .verify_and_set_breakpoint(TypedPath::derive(path), line, None, &source)
                .map_err(|e| DebuggerError::UserMessage(e.to_string()))?;

            let body = serde_json::to_value(BreakpointEventBody {
                breakpoint: Breakpoint {
                    id: Some(verified.address as i64),
                    verified: true,
                    line: verified.source_location.line.map(|l| l as i64),
                    source: Some(source),
                    message: Some(format!("Source breakpoint at {:#010X}", verified.address)),
                    column: None,
                    end_column: None,
                    end_line: None,
                    instruction_reference: None,
                    offset: None,
                    reason: None,
                },
                reason: "new".to_string(),
            })
            .ok();

            adapter.dyn_send_event("breakpoint", body)?;

            Ok(EvalResponse::Message(format!(
                "Breakpoint set at {:#010X}",
                verified.address
            )))
        }
    }
}

fn clear_breakpoint(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let Some(token) = command_arguments.split_whitespace().next() else {
        return Err(DebuggerError::UserMessage(
            "Missing argument. See `help`.".to_string(),
        ));
    };

    let address = match parse_breakpoint_location(token)? {
        BreakpointLocation::Address(addr) => addr,

        BreakpointLocation::FileLine { path, line } => {
            let Some(ref debug_info) = target_core.core_data.debug_info else {
                return Err(DebuggerError::UserMessage(
                    "Cannot resolve file:line without debug information.".to_string(),
                ));
            };
            debug_info
                .get_breakpoint_location(TypedPath::derive(path), line, None)
                .map_err(|e| {
                    DebuggerError::UserMessage(format!("Cannot resolve {path}:{line}: {e}"))
                })?
                .address
        }
    };

    if !target_core.clear_breakpoint(address)? {
        return Err(DebuggerError::UserMessage(format!(
            "No breakpoint found at {address:#x}."
        )));
    }

    let body = serde_json::to_value(BreakpointEventBody {
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
    .ok();

    adapter.dyn_send_event("breakpoint", body)?;

    Ok(EvalResponse::Message("Breakpoint cleared".to_string()))
}

fn source_from_path(path: &str) -> Source {
    Source {
        name: std::path::Path::new(path)
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned),
        path: Some(path.to_string()),
        source_reference: None,
        presentation_hint: None,
        origin: None,
        sources: None,
        adapter_data: None,
        checksums: None,
    }
}
