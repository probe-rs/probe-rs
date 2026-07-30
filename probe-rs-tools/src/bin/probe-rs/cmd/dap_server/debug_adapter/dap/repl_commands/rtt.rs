use linkme::distributed_slice;

use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::EvaluateArguments,
            repl_commands::{
                EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn, need_subcommand,
            },
            repl_types::ReplCommandArgs,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreData,
};

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
        handler: async_fn!(rtt_write),
    }],
    args: &[],
    handler: async_fn!(need_subcommand),
};

async fn rtt_write<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    input: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let (channel_id, data) = input.split_once(' ').ok_or_else(|| {
        DebuggerError::UserMessage("Expected input format: <channel_id> <data>".to_string())
    })?;

    let channel_id = channel_id
        .parse()
        .map_err(|_| DebuggerError::UserMessage("Channel ID must be a number".to_string()))?;

    let mut data = data.to_string();
    data.push('\n');

    let rtt = core_data
        .rtt_connection
        .as_mut()
        .ok_or_else(|| DebuggerError::UserMessage("Not connected to RTT".to_string()))?;

    rtt.client
        .write_down_async(channel_id, data.into_bytes())
        .await
        .map_err(|e| {
            DebuggerError::UserMessage(format!("Failed to write to channel {channel_id}: {e}"))
        })?;

    Ok(EvalResponse::Message(String::new()))
}
