use std::{fmt::Write, path::Path};

use linkme::distributed_slice;

use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::EvaluateArguments,
            repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn},
            repl_types::ReplCommandArgs,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreData,
};
use crate::rpc::functions::stack_trace::StackTraceFrame;
use crate::util::cli::format_stack_frame;

#[distributed_slice(REPL_COMMANDS)]
static BACKTRACE: ReplCommand = ReplCommand {
    command: "bt",
    requires_target_halted: true,
    sub_commands: &[ReplCommand {
        command: "yaml",
        help_text: "Print all information about the backtrace of the current thread to a local file in YAML format.",
        requires_target_halted: true,
        sub_commands: &[],
        args: &[ReplCommandArgs::Required(
            "path (e.g. my_dir/backtrace.yaml)",
        )],
        handler: async_fn!(save_backtrace_to_yaml),
    }],
    help_text: "Print the backtrace of the current thread.",
    args: &[],
    handler: async_fn!(print_backtrace),
};

async fn print_backtrace<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    debug_adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let colorize = Some(debug_adapter.supports_ansi_styling);

    let mut response_message = String::new();
    for (i, frame) in core_data.stack_frames.iter().enumerate() {
        #[allow(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        writeln!(
            &mut response_message,
            "    Frame {}: {}",
            i + 1,
            format_stack_frame(&StackTraceFrame::from(frame), colorize)
        )
        .unwrap();
    }

    Ok(EvalResponse::Message(response_message))
}

async fn save_backtrace_to_yaml<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _debug_adapter: &'a mut DebugAdapter<dyn ProtocolAdapter + 'a>,
) -> EvalResult {
    let mut args = command_arguments.split_whitespace();
    let write_to_file = args.next().map(Path::new);

    use insta::_macro_support as insta_yaml;
    let yaml_data = insta_yaml::serialize_value(
        &core_data.stack_frames,
        insta_yaml::SerializationFormat::Yaml,
    );

    let response_message = if let Some(location) = write_to_file {
        std::fs::write(location, yaml_data)
            .map_err(|e| DebuggerError::UserMessage(format!("{e:?}")))?;
        format!("Stacktrace successfully stored at {location:?}.")
    } else {
        yaml_data
    };

    Ok(EvalResponse::Message(response_message))
}
