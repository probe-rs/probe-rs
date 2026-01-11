use std::fmt::Write;

use linkme::distributed_slice;
use probe_rs::{CoreInterface, RegisterValue};
use probe_rs_debug::VariableName;

use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::dap::{
        dap_types::Response,
        repl_commands::{REPL_COMMANDS, ReplCommand},
        repl_commands_helpers::get_local_variable,
        repl_types::{GdbFormat, GdbNuf, ReplCommandArgs},
    },
};

#[distributed_slice(REPL_COMMANDS)]
static INFO: ReplCommand = ReplCommand {
    command: "info",
    help_text: "Information of specified program data.",
    sub_commands: &[
        ReplCommand {
            command: "frame",
            help_text: "Describe the current frame, or the frame at the specified (hex) address.",
            sub_commands: &[],
            args: &[ReplCommandArgs::Optional("address")],
            // TODO: This is easy to implement ... just requires deciding how to format the output.
            handler: |_, _, _| Err(DebuggerError::Unimplemented),
        },
        ReplCommand {
            command: "locals",
            help_text: "List local variables of the selected frame.",
            sub_commands: &[],
            args: &[],
            handler: |target_core, _, evaluate_arguments| {
                let gdb_nuf = GdbNuf {
                    format_specifier: GdbFormat::Native,
                    ..Default::default()
                };
                let variable_name = VariableName::LocalScopeRoot;
                get_local_variable(evaluate_arguments, target_core, variable_name, gdb_nuf)
            },
        },
        ReplCommand {
            command: "reg",
            help_text: "List registers in the selected frame.",
            sub_commands: &[],
            args: &[ReplCommandArgs::Optional("register name")],
            handler: |target_core, command_arguments, _| {
                let register_name = command_arguments.trim();
                let regs = target_core.core.registers().all_registers().filter(|reg| {
                    if register_name.is_empty() {
                        true
                    } else {
                        reg.name().eq_ignore_ascii_case(register_name)
                    }
                });

                let mut results = vec![];
                for reg in regs {
                    let reg_value: RegisterValue = target_core.core.read_core_reg(reg.id())?;
                    results.push((format!("{reg}:"), reg_value.to_string()));
                }

                if results.is_empty() {
                    return Err(DebuggerError::UserMessage(format!(
                        "No registers found matching {register_name:?}. See the `help` command for more information."
                    )));
                }

                Ok(Response {
                    command: "registers".to_string(),
                    success: true,
                    message: Some(reg_table(&results, 80)),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                })
            },
        },
        ReplCommand {
            command: "var",
            help_text: "List all static variables.",
            sub_commands: &[],
            args: &[],
            // TODO: This is easy to implement ... just requires deciding how to format the output.
            handler: |_, _, _| Err(DebuggerError::Unimplemented),
        },
        ReplCommand {
            command: "break",
            help_text: "List all breakpoints.",
            sub_commands: &[],
            args: &[],
            handler: |target_core, _, _| {
                let breakpoint_addrs = target_core
                    .core
                    .hw_breakpoints()?
                    .into_iter()
                    .enumerate()
                    .filter_map(|(idx, bpt)| bpt.map(|bpt| (idx, bpt)));

                let mut response_message = String::new();

                for (idx, bpt) in breakpoint_addrs {
                    #[expect(clippy::unwrap_used, reason = "Writing to a string is infallible")]
                    writeln!(&mut response_message, "Breakpoint #{idx} @ {bpt:#010X}").unwrap();
                }

                if response_message.is_empty() {
                    response_message.push_str("No breakpoints set.");
                }

                Ok(Response {
                    command: "breakpoints".to_string(),
                    success: true,
                    message: Some(response_message),
                    type_: "response".to_string(),
                    request_seq: 0,
                    seq: 0,
                    body: None,
                })
            },
        },
    ],
    args: &[],
    handler: |_, _, _| {
        Err(DebuggerError::UserMessage("Please provide one of the required subcommands. See the `help` command for more information.".to_string()))
    },
};

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
