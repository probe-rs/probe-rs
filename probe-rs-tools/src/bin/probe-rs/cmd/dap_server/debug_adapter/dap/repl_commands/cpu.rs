use crate::cmd::dap_server::debug_adapter::dap::{
    core_status::DapStatus, dap_types::Response, repl_commands::REPL_COMMANDS,
    repl_commands::ReplCommand,
};

use linkme::distributed_slice;
use probe_rs::{CoreStatus, HaltReason};

#[distributed_slice(REPL_COMMANDS)]
static CONTINUE: ReplCommand = ReplCommand {
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
};

#[distributed_slice(REPL_COMMANDS)]
static RESET: ReplCommand = ReplCommand {
    command: "reset",
    help_text: "Reset the target",
    sub_commands: &[],
    args: &[],
    handler: |target_core, _, _| {
        let core_info = target_core.reset_and_halt()?;

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
};

#[distributed_slice(REPL_COMMANDS)]
static STEP: ReplCommand = ReplCommand {
    command: "step",
    help_text: "Step one instruction",
    sub_commands: &[],
    args: &[],
    handler: |target_core, _, _| {
        let core_info = target_core.core.step()?;

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
};
