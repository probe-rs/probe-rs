use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};
use probe_rs_debug::{ColumnType, SourceLocation, VerifiedBreakpoint};
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
        request_helpers::get_dap_source,
    },
    server::core_data::CoreData,
    server::session_data::{ActiveBreakpoint, BreakpointType, SourceLocationScope},
};
use crate::util::style::format_location;
use probe_rs_rpc::breakpoints::SourceBreakpointLocation;

#[distributed_slice(REPL_COMMANDS)]
static BREAK: ReplCommand = ReplCommand {
    command: "break",
    help_text: "Set a breakpoint at a location, or halt the target if unspecified.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("address | file:line[:column]")],
    handler: async_fn!(create_breakpoint),
};

#[distributed_slice(REPL_COMMANDS)]
static CLEAR: ReplCommand = ReplCommand {
    command: "clear",
    help_text: "Clear a breakpoint.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Required("address | file:line[:column]")],
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

/// Parse `[*]<address>`, `<file>:<line>`, or `<file>:<line>:<column>` from a single REPL token.
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

    if let Ok(MemoryAddress(address)) = input.try_into() {
        return Ok(BreakpointLocation::Address(address));
    }

    Err(DebuggerError::UserMessage(format!(
        "Invalid argument {input:?}. Expected `[*]<address>` or `<file>:<line>[:<column>]`. See `help`."
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
                .short_long_status(backend, Some(core_info.pc), adapter.supports_ansi_styling)
                .await
                .1,
        ));
    }

    let Some(token) = command_arguments.split_whitespace().next() else {
        return Err(DebuggerError::UserMessage(
            "Missing argument. See `help`.".to_string(),
        ));
    };

    let (address, source_location, breakpoint_type) = match parse_breakpoint_location(token)? {
        BreakpointLocation::Address(address) => {
            let source_location = backend.source_location(address).await;

            (
                address,
                source_location,
                BreakpointType::InstructionBreakpoint,
            )
        }

        BreakpointLocation::FileLine { path, line, column } => {
            let VerifiedBreakpoint {
                address,
                source_location,
            } = resolve_one_source_breakpoint(backend, path, line, column).await?;
            let breakpoint_type = BreakpointType::SourceBreakpoint {
                source: Box::new(source_from_path(path)),
                location: SourceLocationScope::Specific(source_location.clone()),
            };

            (address, Some(source_location), breakpoint_type)
        }
    };

    backend.set_hw_breakpoint(core_index, address).await?;
    core_data.breakpoints.push(ActiveBreakpoint {
        breakpoint_type,
        address,
    });

    let body = serde_json::to_value(BreakpointEventBody {
        breakpoint: Breakpoint {
            id: Some(address as i64),
            verified: true,
            line: source_location
                .as_ref()
                .and_then(|loc| loc.line)
                .map(|l| l as i64),
            column: source_location
                .as_ref()
                .and_then(|loc| loc.column)
                .map(|col| match col {
                    ColumnType::LeftEdge => 0_i64,
                    ColumnType::Column(c) => c as i64,
                }),
            source: source_location.as_ref().and_then(get_dap_source),
            message: Some(breakpoint_set_message(
                address,
                source_location.as_ref(),
                false,
            )),
            instruction_reference: Some(format!("{address:#010x}")),
            end_column: None,
            end_line: None,
            offset: None,
            reason: None,
        },
        reason: "new".to_string(),
    })
    .ok();
    adapter.dyn_send_event("breakpoint", body)?;

    Ok(EvalResponse::Message(breakpoint_set_message(
        address,
        source_location.as_ref(),
        adapter.supports_ansi_styling,
    )))
}

fn breakpoint_set_message(
    address: u64,
    source_location: Option<&SourceLocation>,
    colorize: bool,
) -> String {
    format!(
        "Breakpoint set at {}",
        format_location(address, source_location, colorize)
    )
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

    #[test]
    fn reports_the_source_location_of_a_breakpoint() {
        let location = SourceLocation {
            path: TypedPath::derive("/src/main.rs").to_path_buf(),
            line: Some(42),
            column: Some(ColumnType::Column(7)),
            address: Some(0x2000),
        };

        pretty_assertions::assert_eq!(
            breakpoint_set_message(0x2000, Some(&location), false),
            "Breakpoint set at 0x00002000 (/src/main.rs:42:7)"
        );
        pretty_assertions::assert_eq!(
            breakpoint_set_message(0x2000, None, true),
            "Breakpoint set at \u{1b}[33m0x00002000\u{1b}[0m"
        );
    }

    #[test]
    fn parses_addresses_with_and_without_the_star_prefix() {
        for input in ["*0x2000", "0x2000", "8192"] {
            let BreakpointLocation::Address(address) = parse_breakpoint_location(input).unwrap()
            else {
                panic!("expected address breakpoint for {input:?}");
            };

            assert_eq!(address, 0x2000);
        }
    }
}
