use probe_rs_debug::{ObjectRef, StackFrame, VariableName};

use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::dap::repl_commands::{EvalResponse, EvalResult},
};

use super::{
    dap_types::{
        CompletionItem, CompletionItemType, CompletionsArguments, EvaluateArguments,
        EvaluateResponseBody, Variable,
    },
    repl_commands::ReplCommand,
    repl_types::*,
};

/// Fetch variables for a scope identified by its DAP presentation hint.
pub(crate) async fn scope_variables(
    backend: &mut RpcBackend,
    core_index: usize,
    frame_id: u32,
    presentation_hint: &str,
) -> Result<Option<Vec<Variable>>, DebuggerError> {
    let scopes = backend.scopes(core_index, frame_id).await?;
    let Some(scope) = scopes
        .into_iter()
        .find(|scope| scope.presentation_hint.as_deref() == Some(presentation_hint))
    else {
        return Ok(None);
    };
    let variables_reference = u32::try_from(scope.variables_reference).map_err(|_| {
        DebuggerError::UserMessage(format!(
            "Invalid {presentation_hint}-variable reference returned by server."
        ))
    })?;
    Ok(Some(
        backend
            .variables(core_index, variables_reference, None)
            .await?,
    ))
}

pub(crate) fn stack_frame_id(stack_frame: &StackFrame) -> Result<u32, DebuggerError> {
    i64::from(stack_frame.id)
        .try_into()
        .map_err(|_| DebuggerError::UserMessage("Invalid selected frame id.".to_string()))
}

pub(crate) fn select_frame(
    frames: &[StackFrame],
    requested_frame_id: Option<i64>,
    address: &str,
) -> Result<usize, DebuggerError> {
    let address = address.trim();
    if !address.is_empty() {
        let address = parse_int::parse::<u64>(address)
            .map_err(|error| DebuggerError::UserMessage(error.to_string()))?;
        return frames
            .iter()
            .position(|frame| TryInto::<u64>::try_into(frame.pc).ok() == Some(address))
            .ok_or_else(|| {
                DebuggerError::UserMessage(format!(
                    "No cached stack frame found at address {address:#x}."
                ))
            });
    }

    match requested_frame_id {
        Some(frame_id) => frames
            .iter()
            .position(|frame| frame.id == ObjectRef::from(frame_id))
            .ok_or_else(|| {
                DebuggerError::UserMessage(format!("No stack frame found for id {frame_id}."))
            }),
        None => (!frames.is_empty())
            .then_some(0)
            .ok_or_else(|| DebuggerError::UserMessage("No frame selected.".to_string())),
    }
}

/// Fetch and format local variables from the server-owned variable cache.
pub(crate) async fn get_local_variable(
    backend: &mut RpcBackend,
    evaluate_arguments: &EvaluateArguments,
    core_data: &crate::cmd::dap_server::server::core_data::CoreData,
    variable_name: VariableName,
    gdb_nuf: GdbNuf,
) -> EvalResult {
    let frame_ref = evaluate_arguments.frame_id.map(ObjectRef::from);

    let stack_frame = match frame_ref {
        Some(frame_id) => core_data
            .stack_frames
            .iter()
            .find(|stack_frame| stack_frame.id == frame_id),
        None => {
            // Use the current frame_id
            core_data.stack_frames.first()
        }
    };

    // Make sure we have a valid StackFrame
    let Some(stack_frame) = stack_frame else {
        return Err(DebuggerError::UserMessage("No frame selected.".to_string()));
    };

    let frame_id = stack_frame_id(stack_frame)?;

    if let VariableName::Named(name) = variable_name {
        let response = backend
            .evaluate_repl_variable(core_data.core_index, frame_id, name.clone())
            .await?;
        return Ok(EvalResponse::Body(format_repl_variable(
            &name, response, gdb_nuf,
        )));
    }

    let Some(variables) =
        scope_variables(backend, core_data.core_index, frame_id, "locals").await?
    else {
        return Err(DebuggerError::UserMessage(format!(
            "No variables available for frame: {:?}.",
            stack_frame.function_name
        )));
    };

    Ok(EvalResponse::Body(format_repl_variables(
        &variables, &gdb_nuf,
    )))
}

fn empty_evaluate_response() -> EvaluateResponseBody {
    EvaluateResponseBody {
        result: String::new(),
        variables_reference: 0,
        named_variables: None,
        indexed_variables: None,
        memory_reference: None,
        type_: None,
        presentation_hint: None,
        value_location_reference: None,
    }
}

pub(crate) fn format_repl_variables(
    variables: &[Variable],
    gdb_nuf: &GdbNuf,
) -> EvaluateResponseBody {
    let mut response = empty_evaluate_response();
    for variable in variables {
        append_repl_variable(&mut response, variable, gdb_nuf);
    }
    response
}

fn append_repl_variable(
    response: &mut EvaluateResponseBody,
    variable: &Variable,
    gdb_nuf: &GdbNuf,
) {
    if gdb_nuf.format_specifier == GdbFormat::DapReference {
        response.memory_reference = variable.memory_reference.clone();
        response.result = format!("{} : {} ", variable.name, variable.value);
        response.type_ = variable.type_.clone();
        response.variables_reference = variable.variables_reference;
        response.named_variables = variable.named_variables;
        response.indexed_variables = variable.indexed_variables;
    } else {
        response.result.push_str(&format!(
            "\n{} [{} @ {}]: {} ",
            variable.name,
            variable.type_.as_deref().unwrap_or("<unknown>"),
            variable.memory_reference.as_deref().unwrap_or("<unknown>"),
            variable.value
        ));
    }
}

fn format_repl_variable(
    name: &str,
    mut response: EvaluateResponseBody,
    gdb_nuf: GdbNuf,
) -> EvaluateResponseBody {
    let value = std::mem::take(&mut response.result);
    if gdb_nuf.format_specifier == GdbFormat::DapReference {
        response.result = format!("{name} : {value} ");
    } else {
        response.result = format!(
            "\n{name} [{} @ {}]: {value} ",
            response.type_.as_deref().unwrap_or("<unknown>"),
            response.memory_reference.as_deref().unwrap_or("<unknown>")
        );
    }
    response
}

/// Read memory at the specified address (hex), using the [`GdbNuf`] specifiers to determine size and format.
pub(crate) async fn memory_read_async(
    backend: &mut RpcBackend,
    core_index: usize,
    address: u64,
    gdb_nuf: GdbNuf,
) -> EvalResult {
    if gdb_nuf.format_specifier == GdbFormat::Instruction {
        let assembly_lines = backend
            .disassemble(core_index, address, 0, 0, gdb_nuf.unit_count as i64)
            .await?;
        if assembly_lines.is_empty() {
            return Err(DebuggerError::UserMessage(format!(
                "Cannot disassemble memory at address {address:#010x}"
            )));
        }
        let mut formatted_output = String::new();
        for assembly_line in &assembly_lines {
            formatted_output.push_str(&assembly_line.to_string());
        }

        Ok(EvalResponse::Message(formatted_output))
    } else {
        let memory = backend
            .read_memory_8(core_index, address, gdb_nuf.get_size())
            .await
            .map_err(|err| {
                DebuggerError::UserMessage(format!(
                    "Cannot read memory at address {address:#010x}: {err:?}"
                ))
            })?;
        Ok(EvalResponse::Message(
            GdbNufMemoryResult {
                nuf: &gdb_nuf,
                memory: &memory,
            }
            .to_string(),
        ))
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
            if command_root.is_empty() {
                command_root = command_piece.to_string();
            }
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
        dap_types::EvaluateResponseBody,
        repl_commands::REPL_COMMANDS,
        repl_commands_helpers::{
            build_completions, build_expanded_commands, format_repl_variable, format_repl_variables,
        },
        repl_types::{GdbFormat, GdbNuf},
    };

    #[test]
    fn formats_rpc_variable_for_repl() {
        let response = EvaluateResponseBody {
            result: "42".to_string(),
            type_: Some("i32".to_string()),
            variables_reference: 7,
            named_variables: Some(1),
            indexed_variables: Some(0),
            memory_reference: Some("0x20000000".to_string()),
            presentation_hint: None,
            value_location_reference: None,
        };

        let formatted = format_repl_variable(
            "answer",
            response,
            GdbNuf {
                format_specifier: GdbFormat::Native,
                ..Default::default()
            },
        );

        assert_eq!(formatted.result, "\nanswer [i32 @ 0x20000000]: 42 ");
        assert_eq!(formatted.variables_reference, 7);
    }

    #[test]
    fn formats_rpc_variable_list_for_repl() {
        let variables = vec![super::Variable {
            name: "STATIC_COUNT".to_string(),
            value: "3".to_string(),
            type_: Some("u32".to_string()),
            memory_reference: Some("0x20000010".to_string()),
            variables_reference: 0,
            evaluate_name: None,
            indexed_variables: Some(0),
            named_variables: Some(0),
            presentation_hint: None,
            declaration_location_reference: None,
            value_location_reference: None,
        }];

        let formatted = format_repl_variables(
            &variables,
            &GdbNuf {
                format_specifier: GdbFormat::Native,
                ..Default::default()
            },
        );

        assert_eq!(formatted.result, "\nSTATIC_COUNT [u32 @ 0x20000010]: 3 ");
    }

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
                    "break [*address | file:line[:column]]: Set a breakpoint at a location, or halt the target if unspecified.",
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
                "break [*address | file:line[:column]]: Set a breakpoint at a location, or halt the target if unspecified.",
            )],
        );
        assert_completion_result(
            "break",
            &[(
                "break ",
                "break [*address | file:line[:column]]: Set a breakpoint at a location, or halt the target if unspecified.",
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
