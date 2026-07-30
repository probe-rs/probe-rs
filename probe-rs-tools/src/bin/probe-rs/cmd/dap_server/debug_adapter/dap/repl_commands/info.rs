use std::fmt::Write;

use linkme::distributed_slice;
use probe_rs::{CoreRegister, RegisterId};
use probe_rs_debug::{ColumnType, StackFrame, VariableName};

use crate::cmd::dap_server::{
    DebuggerError,
    backend::rpc::RpcBackend,
    debug_adapter::dap::{
        adapter::DebugAdapter,
        dap_types::EvaluateArguments,
        repl_commands::{
            EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand, async_fn, need_subcommand,
        },
        repl_commands_helpers::{
            format_repl_variables, get_local_variable, scope_variables, select_frame,
            stack_frame_id,
        },
        repl_types::{GdbFormat, GdbNuf, ReplCommandArgs},
    },
    server::core_data::CoreData,
};

#[distributed_slice(REPL_COMMANDS)]
static INFO: ReplCommand = ReplCommand {
    command: "info",
    help_text: "Information of specified program data.",
    requires_target_halted: false,
    sub_commands: &[
        ReplCommand {
            command: "frame",
            help_text: "Describe the current frame, or the frame at the specified (hex) address.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[ReplCommandArgs::Optional("address")],
            handler: async_fn!(info_frame),
        },
        ReplCommand {
            command: "locals",
            help_text: "List local variables of the selected frame.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(info_locals),
        },
        ReplCommand {
            command: "reg",
            help_text: "List registers in the selected frame.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[ReplCommandArgs::Optional("register name")],
            handler: async_fn!(print_registers),
        },
        ReplCommand {
            command: "var",
            help_text: "List all static variables.",
            requires_target_halted: true,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(info_static_variables),
        },
        ReplCommand {
            command: "break",
            help_text: "List all breakpoints.",
            requires_target_halted: false,
            sub_commands: &[],
            args: &[],
            handler: async_fn!(print_breakpoints),
        },
    ],
    args: &[],
    handler: async_fn!(need_subcommand),
};

async fn info_frame<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let frame_index = select_frame(
        &core_data.stack_frames,
        evaluate_arguments.frame_id,
        command_arguments,
    )?;
    Ok(EvalResponse::Message(format_frame(
        frame_index,
        &core_data.stack_frames[frame_index],
    )))
}

async fn info_static_variables<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let frame_index = select_frame(&core_data.stack_frames, evaluate_arguments.frame_id, "")?;
    let frame_id = stack_frame_id(&core_data.stack_frames[frame_index])?;
    let Some(variables) =
        scope_variables(backend, core_data.core_index, frame_id, "statics").await?
    else {
        return Ok(EvalResponse::Message(
            "No static variables available.".to_string(),
        ));
    };
    if variables.is_empty() {
        return Ok(EvalResponse::Message("No static variables.".to_string()));
    }

    Ok(EvalResponse::Body(format_repl_variables(
        &variables,
        &GdbNuf {
            format_specifier: GdbFormat::Native,
            ..Default::default()
        },
    )))
}

fn format_frame(index: usize, frame: &StackFrame) -> String {
    let mut response = format!(
        "Frame {}: {} @ {}",
        index + 1,
        frame.function_name,
        frame.pc
    );
    if frame.is_inlined {
        response.push_str(" (inline)");
    }
    if let Some(location) = &frame.source_location {
        #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        write!(
            &mut response,
            "\nSource: {}",
            location.path.to_path().display()
        )
        .unwrap();
        if let Some(line) = location.line {
            #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
            write!(&mut response, ":{line}").unwrap();
            if let Some(ColumnType::Column(column)) = location.column {
                #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
                write!(&mut response, ":{column}").unwrap();
            }
        }
    }
    if let Some(frame_base) = frame.frame_base {
        #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        write!(&mut response, "\nFrame base: {frame_base:#x}").unwrap();
    }
    if let Some(cfa) = frame.canonical_frame_address {
        #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        write!(&mut response, "\nCanonical frame address: {cfa:#x}").unwrap();
    }
    response
}

async fn print_registers<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let core_index = core_data.core_index;
    let register_name = command_arguments.trim();
    let registers = backend.core_metadata[core_index]
        .registers
        .all_registers()
        .filter(|reg| register_name.is_empty() || reg.name().eq_ignore_ascii_case(register_name))
        .collect::<Vec<&'static CoreRegister>>();
    if registers.is_empty() {
        return Err(DebuggerError::UserMessage(format!(
            "No registers found matching {register_name:?}. See the `help` command for more information."
        )));
    }

    let ids: Vec<RegisterId> = registers.iter().map(|r| r.id()).collect();
    let values = backend.read_core_registers(core_index, ids).await?;

    let mut results = vec![];
    let mut failures = vec![];
    for (reg, value) in registers.into_iter().zip(values) {
        match value {
            Some(reg_value) => results.push((format!("{reg}:"), reg_value.to_string())),
            None => failures.push((reg.to_string(), "unreadable".to_string())),
        }
    }

    let mut response_message = String::new();

    if !failures.is_empty() {
        response_message.push_str("Failed to read the following registers:");
        for (reg_name, error) in &failures {
            #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
            writeln!(&mut response_message, "{reg_name}: {error}").unwrap();
        }
    }
    if !results.is_empty() {
        if !response_message.is_empty() {
            response_message.push('\n');
        }
        response_message.push_str(&reg_table(&results, 80));
    }

    Ok(EvalResponse::Message(response_message))
}

async fn print_breakpoints<'a>(
    _backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    _evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let mut response_message = String::new();
    for (idx, ab) in core_data.breakpoints.iter().enumerate() {
        #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
        writeln!(
            &mut response_message,
            "Breakpoint #{idx} @ {:010X}",
            ab.address
        )
        .unwrap();
    }

    if response_message.is_empty() {
        response_message.push_str("No breakpoints set.");
    }

    Ok(EvalResponse::Message(response_message))
}

async fn info_locals<'a>(
    backend: &'a mut RpcBackend,
    core_data: &'a mut CoreData,
    _command_arguments: &'a str,
    evaluate_arguments: &'a EvaluateArguments,
    _adapter: &'a mut DebugAdapter,
) -> EvalResult {
    let gdb_nuf = GdbNuf {
        format_specifier: GdbFormat::Native,
        ..Default::default()
    };
    let variable_name = VariableName::LocalScopeRoot;
    get_local_variable(
        backend,
        evaluate_arguments,
        core_data,
        variable_name,
        gdb_nuf,
    )
    .await
}

fn reg_table(results: &[(String, String)], max_line_length: usize) -> String {
    let mut max_reg_name_width = 0;
    let mut max_value_width = 0;

    // Calculate the maximum width of the register names and values
    for (reg_name, reg_value) in results {
        max_reg_name_width = max_reg_name_width.max(reg_name.len());
        max_value_width = max_value_width.max(reg_value.len());
    }

    let entry_width = max_value_width + max_reg_name_width + 1; // +1 for the space between name and value

    let mut response_message = String::new();
    let mut line_length = 0;
    for (reg_name, reg_value) in results {
        // Check if adding the line would exceed the maximum line length
        if line_length + entry_width > max_line_length {
            // If it does, start a new line
            response_message.push('\n');
            line_length = 0;
        }

        // Add the line to the response message
        if line_length != 0 {
            response_message.push(' ');
        }

        #[expect(
            clippy::unwrap_used,
            reason = "This is safe because we are writing to a string"
        )]
        // Format the line name and value
        write!(
            &mut response_message,
            "{reg_name:<max_reg_name_width$} {reg_value:>max_value_width$}"
        )
        .unwrap();

        line_length += entry_width + 1; // +1 for the space between entries
    }
    response_message
}

#[cfg(test)]
mod test {
    use probe_rs::RegisterValue;
    use probe_rs_debug::{DebugRegisters, ObjectRef, StackFrame};

    fn frame(id: i64, pc: u32, name: &str) -> StackFrame {
        StackFrame {
            id: ObjectRef::from(id),
            function_name: name.to_string(),
            source_location: None,
            registers: DebugRegisters::default(),
            pc: RegisterValue::U32(pc),
            frame_base: Some(0x2000),
            is_inlined: false,
            local_variables: None,
            canonical_frame_address: Some(0x2010),
        }
    }

    #[test]
    fn selects_info_frame_by_dap_id_or_address() {
        let frames = vec![frame(11, 0x1000, "top"), frame(12, 0x1010, "caller")];

        assert_eq!(
            crate::cmd::dap_server::debug_adapter::dap::repl_commands_helpers::select_frame(
                &frames,
                Some(12),
                ""
            )
            .unwrap(),
            1
        );
        assert_eq!(
            crate::cmd::dap_server::debug_adapter::dap::repl_commands_helpers::select_frame(
                &frames, None, "0x1000"
            )
            .unwrap(),
            0
        );
        assert!(
            crate::cmd::dap_server::debug_adapter::dap::repl_commands_helpers::select_frame(
                &frames, None, "0x9999"
            )
            .is_err()
        );
    }

    #[test]
    fn formats_info_frame_metadata() {
        let frame = frame(11, 0x1000, "main");

        pretty_assertions::assert_eq!(
            super::format_frame(0, &frame),
            "Frame 1: main @ 0x00001000\nFrame base: 0x2000\nCanonical frame address: 0x2010"
        );
    }

    #[test]
    fn reg_table_output() {
        let results = vec![
            ("PC/R0:".to_string(), "0x00000000".to_string()),
            ("R1:".to_string(), "0x00000001".to_string()),
            ("R2:".to_string(), "0x00000002".to_string()),
            ("R3:".to_string(), "0x00000003".to_string()),
            ("R4:".to_string(), "0x00000004".to_string()),
            ("R5:".to_string(), "0x00000005".to_string()),
        ];

        pretty_assertions::assert_eq!(
            super::reg_table(&results, 20),
            "PC/R0: 0x00000000\nR1:    0x00000001\nR2:    0x00000002\nR3:    0x00000003\nR4:    0x00000004\nR5:    0x00000005"
        );
        pretty_assertions::assert_eq!(
            super::reg_table(&results, 40),
            "PC/R0: 0x00000000 R1:    0x00000001\nR2:    0x00000002 R3:    0x00000003\nR4:    0x00000004 R5:    0x00000005"
        );
        pretty_assertions::assert_eq!(
            super::reg_table(&results, 80),
            "PC/R0: 0x00000000 R1:    0x00000001 R2:    0x00000002 R3:    0x00000003\nR4:    0x00000004 R5:    0x00000005"
        );
    }
}
