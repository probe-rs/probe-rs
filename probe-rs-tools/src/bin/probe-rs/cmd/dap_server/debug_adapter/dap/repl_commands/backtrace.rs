use std::{
    fmt::{Display, Write},
    path::Path,
};

use linkme::distributed_slice;
use probe_rs_debug::{ColumnType, StackFrame};

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            adapter::DebugAdapter,
            dap_types::EvaluateArguments,
            repl_commands::{EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand},
            repl_types::ReplCommandArgs,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};

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
        handler: save_backtrace_to_yaml,
    }],
    help_text: "Print the backtrace of the current thread.",
    args: &[],
    handler: print_backtrace,
};

struct ReplStackFrame<'a>(&'a StackFrame);

impl Display for ReplStackFrame<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header info for the StackFrame
        write!(f, "{}", self.0.function_name)?;
        if let Some(si) = &self.0.source_location {
            write!(f, "\n\t{}", si.path.to_path().display())?;

            if let (Some(column), Some(line)) = (si.column, si.line) {
                match column {
                    ColumnType::Column(c) => write!(f, ":{line}:{c}")?,
                    ColumnType::LeftEdge => write!(f, ":{line}")?,
                }
            }
        }
        Ok(())
    }
}

fn save_backtrace_to_yaml(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let mut args = command_arguments.split_whitespace();

    let write_to_file = args.next().map(Path::new);

    // Using the `insta` crate to serialize, because they add a couple of transformations to the yaml output,
    // presumably to make it easier to read.
    // In our case, we want this backtrace format to be comparable to the unwind tests
    // in `probe-rs::debug::debuginfo`.
    // The reason for this is that these 'live' backtraces are used to create the 'master' snapshots,
    // which is used to compare against backtraces generated from coredumps.
    use insta::_macro_support as insta_yaml;
    let yaml_data = insta_yaml::serialize_value(
        &target_core.core_data.stack_frames,
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

fn print_backtrace(
    target_core: &mut CoreHandle<'_>,
    _: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let mut response_message = String::new();

    for (i, frame) in target_core.core_data.stack_frames.iter().enumerate() {
        #[allow(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        writeln!(
            &mut response_message,
            "Frame #{}: {}",
            i + 1,
            ReplStackFrame(frame)
        )
        .unwrap();
    }

    Ok(EvalResponse::Message(response_message))
}
