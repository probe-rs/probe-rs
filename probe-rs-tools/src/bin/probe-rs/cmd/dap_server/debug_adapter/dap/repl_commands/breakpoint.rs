use std::time::Duration;

use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::{
        core_status::DapStatus,
        dap_types::{
            Breakpoint, BreakpointEventBody, InstructionBreakpoint, MemoryAddress, Response,
        },
        repl_commands::{REPL_COMMANDS, ReplCommand},
        repl_types::ReplCommandArgs,
        request_helpers::set_instruction_breakpoint,
    },
};

#[distributed_slice(REPL_COMMANDS)]
static BREAK: ReplCommand = ReplCommand {
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
        let Some(address_str) = input_arguments.next().and_then(|arg| arg.strip_prefix('*')) else {
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
};

#[distributed_slice(REPL_COMMANDS)]
static CLEAR: ReplCommand = ReplCommand {
    command: "clear",
    help_text: "Clear a breakpoint",
    sub_commands: &[],
    args: &[ReplCommandArgs::Required("*address")],
    handler: |target_core, args, _| {
        let mut input_arguments = args.split_whitespace();
        let Some(input_argument) = input_arguments.next() else {
            return Err(DebuggerError::UserMessage(
                "Missing breakpoint address to clear. See the `help` command for more information."
                    .to_string(),
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
};
