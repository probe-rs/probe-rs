use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            core_status::DapStatus,
            dap_types::{
                Breakpoint, BreakpointEventBody, EvaluateArguments, InstructionBreakpoint,
                MemoryAddress,
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
    // Stricly speaking, gdb refers to this as an expression, but we only support variables.
    help_text: "Sets a breakpoint specified location, or next instruction if unspecified.",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Optional("*address")],
    handler: create_breakpoint,
};

#[distributed_slice(REPL_COMMANDS)]
static CLEAR: ReplCommand = ReplCommand {
    command: "clear",
    help_text: "Clear a breakpoint",
    requires_target_halted: false,
    sub_commands: &[],
    args: &[ReplCommandArgs::Required("*address")],
    handler: clear_breakpoint,
};

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

    let body = if result.verified {
        // The caller will catch this event body and use it to synch the UI breakpoint list.
        serde_json::to_value(BreakpointEventBody {
            breakpoint: result.clone(),
            reason: "new".to_string(),
        })
        .ok()
    } else {
        None
    };

    // Synch the DAP client UI.
    adapter.dyn_send_event("breakpoint", body)?;

    Ok(EvalResponse::Message(result.message.unwrap_or_else(|| {
        format!("Unexpected error creating breakpoint at {address_str}.")
    })))
}

fn clear_breakpoint(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let mut input_arguments = command_arguments.split_whitespace();
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

    if !target_core.clear_breakpoint(address)? {
        return Err(DebuggerError::UserMessage(format!(
            "Breakpoint not found at address {address:#x}."
        )));
    };

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

    // Synch the DAP client UI.
    adapter.dyn_send_event("breakpoint", body)?;

    Ok(EvalResponse::Message("Breakpoint cleared".to_string()))
}
