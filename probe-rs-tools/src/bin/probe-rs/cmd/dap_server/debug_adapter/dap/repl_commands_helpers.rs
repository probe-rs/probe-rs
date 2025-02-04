use probe_rs::MemoryInterface;
use probe_rs_debug::{ObjectRef, VariableName};

use crate::cmd::dap_server::{server::core_data::CoreHandle, DebuggerError};

use super::{
    dap_types::{
        CompletionItem, CompletionItemType, CompletionsArguments, DisassembledInstruction,
        EvaluateArguments, EvaluateResponseBody, Response,
    },
    repl_commands::{ReplCommand, ReplHandler, REPL_COMMANDS},
    repl_types::*,
    request_helpers::disassemble_target_memory,
};

/// Format the `variable` and add it to the `response_body.result` for display to the user.
/// - If the `variable_name` is `VariableName::LocalScopeRoot`, then all local variables will be printed.
pub(crate) fn get_local_variable(
    evaluate_arguments: &EvaluateArguments,
    target_core: &mut CoreHandle,
    variable_name: VariableName,
    gdb_nuf: GdbNuf,
) -> Result<Response, DebuggerError> {
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
    let mut response = Response {
        command: "variables".to_string(),
        success: true,
        message: None,
        type_: "response".to_string(),
        request_seq: 0,
        seq: 0,
        body: None,
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
    };
    response_body.result = "".to_string();
    for variable in variable_list {
        if gdb_nuf.format_specifier == GdbFormat::DapReference {
            response_body.memory_reference = Some(format!("{}", variable.memory_location));
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
    response.message = Some(response_body.result.clone());
    response.body = serde_json::to_value(response_body).ok();
    Ok(response)
}

/// Read memory at the specified address (hex), using the [`GdbNuf`] specifiers to determine size and format.
pub(crate) fn memory_read(
    address: u64,
    gdb_nuf: GdbNuf,
    target_core: &mut CoreHandle,
) -> Result<Response, DebuggerError> {
    let mut response = Response {
        command: "readMemory".to_string(),
        success: true,
        message: None,
        type_: "response".to_string(),
        request_seq: 0,
        seq: 0,
        body: None,
    };
    if gdb_nuf.format_specifier == GdbFormat::Instruction {
        let assembly_lines: Vec<DisassembledInstruction> = disassemble_target_memory(
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
        } else {
            let mut formatted_output = "".to_string();
            for assembly_line in &assembly_lines {
                formatted_output.push_str(&assembly_line.to_string());
            }
            response.message = Some(formatted_output);
        }
    } else {
        let mut memory_result = vec![0u8; gdb_nuf.get_size()];
        match target_core.core.read_8(address, &mut memory_result) {
            Ok(()) => {
                let formatted_output = GdbNufMemoryResult {
                    nuf: &gdb_nuf,
                    memory: &memory_result,
                }
                .to_string();
                response.message = Some(formatted_output);
            }
            Err(err) => {
                return Err(DebuggerError::UserMessage(format!(
                    "Cannot read memory at address {address:#010x}: {err:?}"
                )))
            }
        }
    }
    Ok(response)
}

/// Get a list of command matches, based on the given command piece.
/// The `command_piece` is a valid [`ReplCommand`], which can be either a command or a sub_command.
pub(crate) fn find_commands<'a>(
    repl_commands: &[&'a ReplCommand<ReplHandler>],
    command_piece: &'a str,
) -> Vec<&'a ReplCommand<ReplHandler>> {
    repl_commands
        .iter()
        .filter(move |command| command.command.starts_with(command_piece))
        .copied()
        .collect::<Vec<&ReplCommand<ReplHandler>>>()
}

/// Iteratively builds a list of command matches, based on the given filter.
/// If multiple levels of commands are involved, the ReplCommand::command will be concatenated.
pub(crate) fn build_expanded_commands(
    command_filter: &str,
) -> (String, Vec<&ReplCommand<ReplHandler>>) {
    // Split the given text into a command, optional sub-command, and optional arguments.
    let command_pieces = command_filter.split(&[' ', '/', '*'][..]);

    // Always start building from the top-level commands.
    let mut repl_commands: Vec<&ReplCommand<ReplHandler>> = REPL_COMMANDS.iter().collect();

    let mut command_root = "".to_string();
    for command_piece in command_pieces {
        // Find the matching commands.
        let matches = find_commands(&repl_commands, command_piece);

        // If there is only one match, and it has sub-commands, then we can continue iterating (implicit recursion with new sub-command).
        if matches.len() == 1 {
            if let Some(parent_command) = matches.first() {
                if let Some(sub_commands) = parent_command.sub_commands {
                    // Build up the full command as we iterate ...
                    if !command_root.is_empty() {
                        command_root.push(' ');
                    }
                    command_root.push_str(parent_command.command);
                    repl_commands = sub_commands.iter().collect();
                    continue;
                }
            }
        }

        if matches.is_empty() {
            // If there are no matches, then we can keep the matches from the previous iteration (if there were any).
        } else {
            // If there are multiple matches, or there is only one match with no sub-commands, then we can use the matches.
            repl_commands = matches;
        }
        break;
    }
    (command_root, repl_commands)
}

/// Returns a list of completion items for the REPL, based on matches to the given filter.
pub(crate) fn command_completions(arguments: CompletionsArguments) -> Vec<CompletionItem> {
    let (command_root, command_list) = if arguments.text.is_empty() {
        // If the filter is empty, then we can return all commands.
        (
            arguments.text,
            REPL_COMMANDS
                .iter()
                .collect::<Vec<&ReplCommand<ReplHandler>>>(),
        )
    } else {
        // Iterate over the command pieces, and find the matching commands.
        let (command_root, command_list) = build_expanded_commands(&arguments.text);
        (format!("{command_root} "), command_list)
    };
    command_list
        .iter()
        .map(|command| CompletionItem {
            // Add a space after the command, so that the user can start typing the next command.
            // This space will be trimmed if the user selects to evaluate the command as is.
            label: format!("{command_root}{} ", command.command),
            text: None,
            sort_text: None,
            detail: Some(command.to_string()),
            type_: Some(CompletionItemType::Keyword),
            start: None,
            length: None, //Some(arguments.column),
            selection_start: None,
            selection_length: None,
        })
        .collect()
}
