use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};
use probe_rs_debug::{ColumnType, VerifiedBreakpoint};
use typed_path::TypedPath;

use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::dap::{
        adapter::DebugAdapter,
        core_status::DapStatus,
        dap_types::{Breakpoint, BreakpointEventBody, EvaluateArguments, MemoryAddress, Source},
        repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn},
        repl_types::ReplCommandArgs,
        request_helpers::instruction_breakpoint_response,
    },
    server::core_data::CoreData,
    server::session_data::{ActiveBreakpoint, BreakpointType, SourceLocationScope},
};
use probe_rs_rpc::breakpoints::SourceBreakpointLocation;

#[distributed_slice(REPL_COMMANDS)]
static BREAK: ReplCommand = ReplCommand {
    command: "break",
    help_text: "Set a breakpoint at a location, or halt the target if unspecified.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("*address | file:line[:column]")],
    handler: async_fn!(create_breakpoint),
};

#[distributed_slice(REPL_COMMANDS)]
static CLEAR: ReplCommand = ReplCommand {
    command: "clear",
    help_text: "Clear a breakpoint.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Required("*address | file:line[:column]")],
    handler: async_fn!(clear_breakpoint),
};

enum BreakpointLocation<'a> {
    Address(u64),
    FileLine {
        path: &'a str,
        line: u64,
        column: Option<u64>,
    },
}

/// Parse `*<address>`, `<file>:<line>`, or `<file>:<line>:<column>` from a single REPL token.
///
/// Splitting is done from the right so that Windows drive letters
/// (e.g. `C:\foo.rs:42` or `C:\foo.rs:42:5`) are handled correctly.
fn parse_breakpoint_location(input: &str) -> Result<BreakpointLocation<'_>, DebuggerError> {
    if let Some(addr_str) = input.strip_prefix('*') {
        let MemoryAddress(address) = addr_str.try_into()?;
        return Ok(BreakpointLocation::Address(address));
    }

    if let Some((left, rightmost)) = input.rsplit_once(':')
        && let Ok(rightmost_num) = rightmost.parse::<u64>()
    {
        if let Some((path, middle)) = left.rsplit_once(':')
            && let Ok(line) = middle.parse::<u64>()
        {
            return Ok(BreakpointLocation::FileLine {
                path,
                line,
                column: Some(rightmost_num),
            });
        }

        return Ok(BreakpointLocation::FileLine {
            path: left,
            line: rightmost_num,
            column: None,
        });
    }

    Err(DebuggerError::UserMessage(format!(
        "Invalid argument {input:?}. Expected `*<address>` or `<file>:<line>[:<column>]`. See `help`."
    )))
}

fn source_from_path(path: &str) -> Source {
    Source {
        name: TypedPath::derive(path)
            .file_name()
            .map(|b| String::from_utf8_lossy(b).to_string()),
        path: Some(path.to_string()),
        source_reference: None,
        presentation_hint: None,
        origin: None,
        sources: None,
        adapter_data: None,
        checksums: None,
    }
}

async fn resolve_one_source_breakpoint(
    backend: &mut RpcBackend,
    path: &str,
    line: u64,
    column: Option<u64>,
) -> Result<VerifiedBreakpoint, DebuggerError> {
    backend
        .resolve_source_breakpoints(vec![SourceBreakpointLocation {
            path: path.to_string(),
            line,
            column,
        }])
        .await?
        .pop()
        .ok_or_else(|| {
            DebuggerError::UserMessage(
                "Server returned no source breakpoint resolution.".to_string(),
            )
        })?
        .map_err(DebuggerError::UserMessage)
}

async fn create_breakpoint<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let core_index = core_data.core_index;
    if command_arguments.is_empty() {
        let core_info = adapter.pause_impl_async(backend, core_data).await?;
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
            let set_result = backend.set_hw_breakpoint(core_index, address).await;
            let source_location = if set_result.is_ok() {
                backend
                    .resolve_source_locations(vec![address])
                    .await
                    .ok()
                    .and_then(|mut locations| locations.pop().flatten())
            } else {
                None
            };
            let breakpoint_response = instruction_breakpoint_response(
                address,
                set_result.is_ok(),
                set_result.as_ref().err().map(|e| e.to_string()).as_deref(),
                source_location.as_ref(),
            );
            let body = if breakpoint_response.verified {
                serde_json::to_value(BreakpointEventBody {
                    breakpoint: breakpoint_response.clone(),
                    reason: "new".to_string(),
                })
                .ok()
            } else {
                None
            };
            adapter.dyn_send_event("breakpoint", body)?;
            Ok(EvalResponse::Message(
                breakpoint_response.message.unwrap_or_else(|| {
                    format!("Unexpected error creating breakpoint at {address:#x}.")
                }),
            ))
        }

        BreakpointLocation::FileLine { path, line, column } => {
            let VerifiedBreakpoint {
                address,
                source_location,
            } = resolve_one_source_breakpoint(backend, path, line, column).await?;
            backend.set_hw_breakpoint(core_index, address).await?;
            let source = source_from_path(path);
            core_data.breakpoints.push(ActiveBreakpoint {
                breakpoint_type: BreakpointType::SourceBreakpoint {
                    source: Box::new(source.clone()),
                    location: SourceLocationScope::Specific(source_location.clone()),
                },
                address,
            });
            let body = serde_json::to_value(BreakpointEventBody {
                breakpoint: Breakpoint {
                    id: Some(address as i64),
                    verified: true,
                    line: source_location.line.map(|l| l as i64),
                    source: Some(source),
                    message: Some(format!("Source breakpoint at {:#010X}", address)),
                    column: source_location.column.map(|col| match col {
                        ColumnType::LeftEdge => 0_i64,
                        ColumnType::Column(c) => c as i64,
                    }),
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
                address
            )))
        }
    }
}

async fn clear_breakpoint<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let core_index = core_data.core_index;
    let Some(token) = command_arguments.split_whitespace().next() else {
        return Err(DebuggerError::UserMessage(
            "Missing argument. See `help`.".to_string(),
        ));
    };

    let address = match parse_breakpoint_location(token)? {
        BreakpointLocation::Address(addr) => addr,
        BreakpointLocation::FileLine { path, line, column } => {
            backend
                .resolve_source_breakpoints(vec![SourceBreakpointLocation {
                    path: path.to_string(),
                    line,
                    column,
                }])
                .await?
                .pop()
                .ok_or_else(|| {
                    DebuggerError::UserMessage(
                        "Server returned no source breakpoint resolution.".to_string(),
                    )
                })?
                .map_err(|error| {
                    DebuggerError::UserMessage(format!("Cannot resolve {path}:{line}: {error}"))
                })?
                .address
        }
    };

    backend
        .clear_hw_breakpoints(core_index, vec![address])
        .await?;
    let before = core_data.breakpoints.len();
    core_data.breakpoints.retain(|ab| ab.address != address);
    let removed = before != core_data.breakpoints.len();
    if !removed {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_source_breakpoint_from_the_right() {
        let BreakpointLocation::FileLine { path, line, column } =
            parse_breakpoint_location(r"C:\src\main.rs:42:7").unwrap()
        else {
            panic!("expected source breakpoint");
        };

        assert_eq!(path, r"C:\src\main.rs");
        assert_eq!(line, 42);
        assert_eq!(column, Some(7));
    }

    #[test]
    fn parses_source_breakpoint_without_column() {
        let BreakpointLocation::FileLine { path, line, column } =
            parse_breakpoint_location("/src/main.rs:42").unwrap()
        else {
            panic!("expected source breakpoint");
        };

        assert_eq!(path, "/src/main.rs");
        assert_eq!(line, 42);
        assert_eq!(column, None);
    }
}
