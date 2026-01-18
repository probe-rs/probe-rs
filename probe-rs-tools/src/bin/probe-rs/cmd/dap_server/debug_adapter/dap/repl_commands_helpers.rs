use probe_rs::MemoryInterface;
use probe_rs_debug::{ObjectRef, VariableName};

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::repl_commands::{EvalResponse, EvalResult},
    server::core_data::CoreHandle,
};

use super::{
    dap_types::{
        CompletionItem, CompletionItemType, CompletionsArguments, EvaluateArguments,
        EvaluateResponseBody,
    },
    repl_commands::ReplCommand,
    repl_types::*,
    request_helpers::disassemble_target_memory,
};

/// Format the `variable` and add it to the `response_body.result` for display to the user.
/// - If the `variable_name` is `VariableName::LocalScopeRoot`, then all local variables will be printed.
pub(crate) fn get_local_variable(
    evaluate_arguments: &EvaluateArguments,
    target_core: &mut CoreHandle<'_>,
    variable_name: VariableName,
    gdb_nuf: GdbNuf,
) -> EvalResult {
    let frame_ref = evaluate_arguments.frame_id.map(ObjectRef::from);

    let stack_frame = match frame_ref {
        Some(frame_id) => target_core
            .core_data
            .stack_frames
            .iter_mut()
            .find(|stack_frame| stack_frame.id == frame_id),
        None => {
            // Use the current frame_id
            target_core.core_data.stack_frames.first_mut()
        }
    };

    // Make sure we have a valid StackFrame
    let Some(stack_frame) = stack_frame else {
        return Err(DebuggerError::UserMessage("No frame selected.".to_string()));
    };

    let Some(variable_cache) = stack_frame.local_variables.as_mut() else {
        return Err(DebuggerError::UserMessage(format!(
            "No variables available for frame: {:?}.",
            stack_frame.function_name
        )));
    };

    let Some(variable) = variable_cache.get_variable_by_name(&variable_name) else {
        return Err(DebuggerError::UserMessage(format!(
            "No variable named {:?} found for frame: {:?}.",
            variable_name, stack_frame.function_name
        )));
    };

    let variable_list = if variable.name == VariableName::LocalScopeRoot {
        variable_cache
            .get_children(variable.variable_key())
            .cloned()
            .collect()
    } else {
        vec![variable]
    };
    let mut response_body = EvaluateResponseBody {
        result: "".to_string(),
        variables_reference: 0,
        named_variables: None,
        indexed_variables: None,
        memory_reference: None,
        type_: None,
        presentation_hint: None,
        value_location_reference: None,
    };

    for variable in variable_list {
        if gdb_nuf.format_specifier == GdbFormat::DapReference {
            response_body.memory_reference = Some(variable.memory_location.to_string());
            response_body.result = format!(
                "{} : {} ",
                variable.name,
                variable.to_string(variable_cache)
            );
            response_body.type_ = Some(variable.type_name());
            response_body.variables_reference = variable.variable_key().into();
        } else {
            response_body.result.push_str(&format!(
                "\n{} [{} @ {}]: {} ",
                variable.name,
                variable.type_name(),
                variable.memory_location,
                variable.to_string(variable_cache)
            ));
        }
    }

    Ok(EvalResponse::Body(response_body))
}

/// Read memory at the specified address (hex), using the [`GdbNuf`] specifiers to determine size and format.
pub(crate) fn memory_read(
    address: u64,
    gdb_nuf: GdbNuf,
    target_core: &mut CoreHandle<'_>,
) -> EvalResult {
    if gdb_nuf.format_specifier == GdbFormat::Instruction {
        let assembly_lines = disassemble_target_memory(
            target_core,
            0_i64,
            0_i64,
            address,
            gdb_nuf.unit_count as i64,
        )?;
        if assembly_lines.is_empty() {
            return Err(DebuggerError::UserMessage(format!(
                "Cannot disassemble memory at address {address:#010x}"
            )));
        }
        let mut formatted_output = "".to_string();
        for assembly_line in &assembly_lines {
            formatted_output.push_str(&assembly_line.to_string());
        }

        Ok(EvalResponse::Message(formatted_output))
    } else {
        let mut memory_result = vec![0u8; gdb_nuf.get_size()];
        match target_core.core.read_8(address, &mut memory_result) {
            Ok(()) => Ok(EvalResponse::Message(
                GdbNufMemoryResult {
                    nuf: &gdb_nuf,
                    memory: &memory_result,
                }
                .to_string(),
            )),
            Err(err) => Err(DebuggerError::UserMessage(format!(
                "Cannot read memory at address {address:#010x}: {err:?}"
            ))),
        }
    }
}

/// Get a list of command matches, based on the given command piece.
/// The `command_piece` is a valid [`ReplCommand`], which can be either a command or a sub_command.
pub(crate) fn find_commands(
    repl_commands: &[ReplCommand],
    command_piece: &str,
) -> Vec<ReplCommand> {
    let mut matches = repl_commands
        .iter()
        .filter(move |command| command.command.starts_with(command_piece))
        .copied()
        .collect::<Vec<_>>();

    // Sort. This will ensure that if there is an exact match, it will be executed.
    matches.sort_by_key(|c| c.command);

    matches
}

/// Iteratively builds a list of command matches, based on the given filter.
/// If multiple levels of commands are involved, the ReplCommand::command will be concatenated.
pub(crate) fn build_expanded_commands<'f>(
    commands: &[ReplCommand],
    command_filter: &'f str,
) -> (String, &'f str, Vec<ReplCommand>) {
    // Split the given text into a command, optional sub-command, and optional arguments.
    let command_pieces = command_filter.split(&[' ', '/', '*'][..]);

    // Always start building from the top-level commands.
    let mut repl_commands = commands.to_vec();

    // The prefix before the command. Does not include the last command piece.
    let mut command_root = String::new();
    // The last command piece.
    let mut last_piece = "";

    // command_root and last_piece are separate to support both command matching and completion listing.

    let piece_count = command_pieces.clone().count();
    for (piece_idx, command_piece) in command_pieces.enumerate() {
        // Find the matching commands.
        let matches = find_commands(&repl_commands, command_piece);

        // If there is only one match, and it has sub-commands, then we can continue iterating (implicit recursion with new sub-command).
        let Some(parent_command) = matches.first() else {
            // If there are no matches, then we can remove some non-matching commands, then we need to stop.
            repl_commands.retain(|cmd| {
                // The first round is special because we have a full set of commands as input, even if the first characters don't match anything.
                let mandatory_prefix = if last_piece.is_empty() {
                    command_piece
                } else {
                    last_piece
                };
                cmd.command.starts_with(mandatory_prefix)
                    && (cmd
                        .sub_commands
                        .iter()
                        .any(|sub_cmd| sub_cmd.command.starts_with(command_piece))
                        || !cmd.args.is_empty())
            });
            break;
        };

        last_piece = command_piece;

        // Since this function is also responsible for generating completions, we need to return all matches.

        if matches.len() == 1
            && !parent_command.sub_commands.is_empty()
            && piece_idx != piece_count - 1
        {
            // Build up the full command as we iterate ...
            if !command_root.is_empty() {
                command_root.push(' ');
            }
            command_root.push_str(command_piece);
            repl_commands = parent_command.sub_commands.to_vec();
        } else {
            // If there are multiple matches, or there is only one match with no
            // sub-commands, then we can use the matches.
            repl_commands = matches;
            break;
        }
    }

    if !command_root.is_empty() {
        command_root.push(' ');
    }

    (command_root, last_piece, repl_commands)
}

fn build_completions(commands: &[ReplCommand], partial: &str) -> Vec<(String, String)> {
    let (command_root, _last_piece, command_list) = build_expanded_commands(commands, partial);
    // Add a space after the command, so that the user can start typing the next command.
    // This space will be trimmed if the user selects to evaluate the command as is.
    command_list
        .iter()
        .map(|command| {
            (
                format!("{command_root}{} ", command.command),
                command.to_string(),
            )
        })
        .collect()
}

/// Returns a list of completion items for the REPL, based on matches to the given filter.
pub(crate) fn command_completions(
    commands: &[ReplCommand],
    arguments: CompletionsArguments,
) -> Vec<CompletionItem> {
    build_completions(commands, &arguments.text)
        .into_iter()
        .map(|(label, detail)| CompletionItem {
            label,
            text: None,
            sort_text: None,
            detail: Some(detail),
            type_: Some(CompletionItemType::Keyword),
            start: None,
            length: None, //Some(arguments.column),
            selection_start: None,
            selection_length: None,
        })
        .collect()
}

#[cfg(test)]
mod test {
    use crate::cmd::dap_server::debug_adapter::dap::{
        repl_commands::REPL_COMMANDS,
        repl_commands_helpers::{build_completions, build_expanded_commands},
    };

    #[test]
    fn finds_matching_command_by_shorthand() {
        let (_root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "4256");
        assert_eq!(commands.len(), 0);
        assert_eq!(last_piece, "");

        let (_root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "br");
        assert_eq!(commands.len(), 1);
        assert_eq!(last_piece, "br");
        assert_eq!(commands[0].command, "break");

        let (_root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "b");
        assert_eq!(commands.len(), 2);
        assert_eq!(last_piece, "b");
        assert_eq!(commands[0].command, "break");

        let (_root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "bt");
        assert_eq!(commands.len(), 1);
        assert_eq!(last_piece, "bt");
        assert_eq!(commands[0].command, "bt");

        let (root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "bt yaml");
        assert_eq!(commands.len(), 1);
        assert_eq!(last_piece, "yaml");
        assert_eq!(commands[0].command, "yaml");
        assert_eq!(root, "bt ");

        // Must not match as "bt yaml"
        let (root, last_piece, commands) = build_expanded_commands(&REPL_COMMANDS, "b yaml");
        assert_eq!(last_piece, "b");
        assert_eq!(commands[0].command, "break");
        assert_eq!(root, ""); // yaml is not a subcommand so we don't include "b" in the command root.
    }

    #[test]
    fn completions() {
        #[track_caller]
        fn assert_completion_result(input: &str, expectation: &[(&str, &str)]) {
            let completions = build_completions(&REPL_COMMANDS, input);
            assert_eq!(completions.len(), expectation.len());
            for (i, (command, description)) in expectation.iter().enumerate() {
                assert_eq!(completions[i].0, *command);
                assert_eq!(completions[i].1, *description);
            }
        }

        assert!(!build_completions(&REPL_COMMANDS, "").is_empty());
        assert!(build_completions(&REPL_COMMANDS, "1234").is_empty());

        assert_completion_result(
            "b",
            &[
                (
                    "break ",
                    "break [*address]: Sets a breakpoint specified location, or next instruction if unspecified.",
                ),
                (
                    "bt ",
                    r#"bt <subcommand>: Print the backtrace of the current thread.
  Subcommands:
  - yaml path (e.g. my_dir/backtrace.yaml): Print all information about the backtrace of the current thread to a local file in YAML format."#,
                ),
            ],
        );
        assert_completion_result(
            "br",
            &[(
                "break ",
                "break [*address]: Sets a breakpoint specified location, or next instruction if unspecified.",
            )],
        );
        assert_completion_result(
            "break",
            &[(
                "break ",
                "break [*address]: Sets a breakpoint specified location, or next instruction if unspecified.",
            )],
        );
        assert_completion_result(
            "bt yaml",
            &[(
                "bt yaml ",
                "yaml path (e.g. my_dir/backtrace.yaml): Print all information about the backtrace of the current thread to a local file in YAML format.",
            )],
        );
        assert_completion_result("bt garbo", &[]);
        assert_completion_result("foo", &[]);
    }
}
