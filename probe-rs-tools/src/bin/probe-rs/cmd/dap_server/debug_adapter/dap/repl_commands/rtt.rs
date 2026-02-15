use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::EvaluateArguments,
            repl_commands::{
                EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, need_subcommand,
            },
            repl_types::ReplCommandArgs,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};

use linkme::distributed_slice;

#[distributed_slice(REPL_COMMANDS)]
static RTT_COMMANDS: ReplCommand = ReplCommand {
    command: "rtt",
    help_text: "Commands to work with Segger RTT",
    requires_target_halted: false,
    sub_commands: &[ReplCommand {
        command: "write",
        help_text: "Write data to a specific channel.",
        requires_target_halted: false,
        sub_commands: &[],
        args: &[
            ReplCommandArgs::Required("channel_id"),
            ReplCommandArgs::Required("data"),
        ],
        handler: write,
    }],
    args: &[],
    handler: need_subcommand,
};

// TODO: add analysis command: print channels, modes, addresses (control block, channel buffers)

fn write(
    target_core: &mut CoreHandle<'_>,
    input: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let (channel_id, data) = input.split_once(' ').ok_or_else(|| {
        DebuggerError::UserMessage("Expected input format: <channel_id> <data>".to_string())
    })?;

    let channel_id = channel_id
        .parse()
        .map_err(|_| DebuggerError::UserMessage("Channel ID must be a number".to_string()))?;

    let Some(rtt) = target_core.core_data.rtt_connection.as_mut() else {
        return Err(DebuggerError::UserMessage(
            "Not connected to RTT".to_string(),
        ));
    };

    rtt.client
        .write_down_channel(&mut target_core.core, channel_id, data)
        .map_err(|e| {
            DebuggerError::UserMessage(format!("Failed to write to channel {}: {}", channel_id, e))
        })?;

    Ok(EvalResponse::Message(String::new()))
}
