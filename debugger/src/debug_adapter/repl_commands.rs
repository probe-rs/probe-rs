use super::{
    dap_adapter::{disassemble_target_memory, DapStatus},
    dap_types::{
        CompletionItem, CompletionItemType, CompletionsArguments, DisassembledInstruction,
        EvaluateArguments, EvaluateResponseBody, VariablePresentationHint,
    },
};
use crate::{
    debugger::{
        core_data::CoreHandle, debug_entry::DebugSessionStatus, session_data::BreakpointType,
    },
    DebuggerError,
};
use probe_rs::{debug::VariableName, CoreStatus, HaltReason, MemoryInterface};
use std::{fmt::Display, time::Duration};

pub(crate) enum ReplCommandArgs {
    Required(&'static str),
    Optional(&'static str),
}

impl Display for ReplCommandArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReplCommandArgs::Required(arg_value) => {
                write!(f, "{}", arg_value)
            }
            ReplCommandArgs::Optional(arg_value) => {
                write!(f, "[{}]", arg_value)
            }
        }
    }
}

/// The handler is a function that takes a reference to the target core, and a reference to the response body.
/// The response body is used to populate the response to the client.
/// The handler can return a [`DebugSessionStatus`], which is used to determine if the debug session should continue, or if it should be terminated.
/// The handler can also return a [`DebuggerError`], which is used to populate the response to the client.
/// The majority of the REPL command results will be populated into the response body.
type H = fn(
    target_core: &mut CoreHandle,
    response: &mut EvaluateResponseBody,
    command_arguments: &str,
    evaluate_arguments: &EvaluateArguments,
) -> Result<DebugSessionStatus, DebuggerError>;

pub(crate) struct ReplCommand<H: 'static> {
    /// The text that the user will type to invoke the command.
    /// - This is case sensitive.
    pub(crate) command: &'static str,
    pub(crate) help_text: &'static str,
    pub(crate) sub_commands: Option<&'static [ReplCommand<H>]>,
    pub(crate) args: Option<&'static [ReplCommandArgs]>,
    pub(crate) handler: H,
}

impl<H> Display for ReplCommand<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ", self.command)?;
        if self.sub_commands.is_some() {
            write!(f, "<subcommand> ")?;
        }
        if let Some(args) = self.args {
            for arg in args {
                write!(f, " {} ", arg)?;
            }
        }
        write!(f, ": {} ", self.help_text)?;
        if let Some(sub_commands) = self.sub_commands {
            write!(f, "\n  Subcommands:")?;
            for sub_command in sub_commands {
                write!(f, "\n  - {}", sub_command)?;
            }
        }
        Ok(())
    }
}

// Limited subset of gdb format specifiers
#[derive(PartialEq)]
enum GdbFormat {
    Binary,
    Hex,
    Instruction,
}

impl TryFrom<&char> for GdbFormat {
    type Error = DebuggerError;

    fn try_from(format: &char) -> Result<Self, Self::Error> {
        match format {
            't' => Ok(GdbFormat::Binary),
            'x' => Ok(GdbFormat::Hex),
            'i' => Ok(GdbFormat::Instruction),
            _ => Err(DebuggerError::ReplError(format!(
                "Invalid format specifier: {format}"
            ))),
        }
    }
}

impl Display for GdbFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GdbFormat::Binary => write!(f, "t(binary)"),
            GdbFormat::Hex => write!(f, "x(hexadecimal)"),
            GdbFormat::Instruction => write!(f, "i(nstruction)"),
        }
    }
}

enum GdbUnit {
    Byte,
    HalfWord,
    Word,
    Giant,
}

impl TryFrom<&char> for GdbUnit {
    type Error = DebuggerError;

    fn try_from(unit_size: &char) -> Result<Self, Self::Error> {
        match unit_size {
            'b' => Ok(GdbUnit::Byte),
            'h' => Ok(GdbUnit::HalfWord),
            'w' => Ok(GdbUnit::Word),
            'g' => Ok(GdbUnit::Giant),
            _ => Err(DebuggerError::ReplError(format!(
                "Invalid unit size: {unit_size}"
            ))),
        }
    }
}

impl GdbUnit {
    fn get_size(&self) -> usize {
        match self {
            GdbUnit::Byte => 1,
            GdbUnit::HalfWord => 2,
            GdbUnit::Word => 4,
            GdbUnit::Giant => 8,
        }
    }
}

impl Display for GdbUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GdbUnit::Byte => write!(f, "b(yte)"),
            GdbUnit::HalfWord => write!(f, "h(alfword)"),
            GdbUnit::Word => write!(f, "w(ord)"),
            GdbUnit::Giant => write!(f, "g(iant)"),
        }
    }
}

struct GdbNuf {
    unit_count: usize,
    unit_specifier: GdbUnit,
    format_specifier: GdbFormat,
}

impl GdbNuf {
    // TODO: If the format_specifier is `instruction` we should return the size of the instruction for the architecture.
    fn get_size(&self) -> usize {
        self.unit_count * self.unit_specifier.get_size()
    }
}

/// TODO: gdb changes the default `format_specifier` everytime x or p is used. For now we will use a static default of `x`.
impl Default for GdbNuf {
    fn default() -> Self {
        Self {
            unit_count: 1,
            unit_specifier: GdbUnit::Word,
            format_specifier: GdbFormat::Hex,
        }
    }
}

impl TryFrom<&str> for GdbNuf {
    type Error = DebuggerError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut nuf = value.to_string();
        let mut unit_specifier: Option<GdbUnit> = None;
        let mut format_specifier: Option<GdbFormat> = None;

        // Decode in reverse order, so that we can deal with variable 'count' characters.
        while let Some(last_char) = nuf.pop() {
            match last_char {
                't' | 'x' | 'i' => {
                    if format_specifier.is_none() {
                        format_specifier = Some(GdbFormat::try_from(&last_char)?);
                    } else {
                        return Err(DebuggerError::ReplError(format!(
                            "Invalid format specifier: {value}"
                        )));
                    }
                }
                'b' | 'h' | 'w' | 'g' => {
                    if unit_specifier.is_none() {
                        unit_specifier = Some(GdbUnit::try_from(&last_char)?);
                    } else {
                        return Err(DebuggerError::ReplError(format!(
                            "Invalid unit specifier: {value}"
                        )));
                    }
                }
                _ => {
                    if last_char.is_numeric() {
                        // The remainder of the string is the unit count.
                        nuf.push(last_char);
                        break;
                    } else {
                        return Err(DebuggerError::ReplError(format!(
                            "Invalid '/Nuf' specifier: {value}"
                        )));
                    }
                }
            }
        }

        let mut result = Self::default();
        if let Some(format_specifier) = format_specifier {
            result.format_specifier = format_specifier;
        }
        if let Some(unit_specifier) = unit_specifier {
            result.unit_specifier = unit_specifier;
        }
        if !nuf.is_empty() {
            result.unit_count = nuf.parse::<usize>().map_err(|error| {
                DebuggerError::ReplError(format!("Invalid unit count specifier: {value} - {error}"))
            })?;
        }

        Ok(result)
    }
}

struct GdbNufMemoryResult<'a> {
    nuf: &'a GdbNuf,
    memory: &'a Vec<u8>,
}

impl Display for GdbNufMemoryResult<'_> {
    // TODO: Consider wrapping lines at 80 characters.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.nuf.format_specifier {
            GdbFormat::Binary => {
                let width = 10_usize;
                for byte in self.memory {
                    write!(f, "{:#0width$b} ", byte)?;
                }
            }
            GdbFormat::Hex => {
                let width = 4_usize;
                for byte in self.memory {
                    write!(f, "{:#0width$x} ", byte)?;
                }
            }
            GdbFormat::Instruction => {
                let width = 4_usize;
                for byte in self.memory {
                    write!(f, "{:#0width$x} ", byte)?;
                }
            }
        }
        Ok(())
    }
}

static REPL_COMMANDS: &[ReplCommand<H>] = &[
    ReplCommand {
        command: "help",
        help_text: "Information about available commands and how to use them.",
        sub_commands: None,
        args: None,
        handler: |_, response_body, _, _| {
            let mut help_text =
                "Usage:\t-Use <Ctrl+Space> to get a list of available commands.".to_string();
            help_text.push_str("\n\t- Use <Up/DownArrows> to navigate through the command list.");
            help_text.push_str("\n\t- Use <Hab> to insert the currently selected command.");
            help_text.push_str("\n\t- Note: This implementation is a subset of gdb commands, and is intended to behave similarly.");
            help_text.push_str("\nAvailable commands:");
            for command in REPL_COMMANDS {
                help_text.push_str(&format!("\n{}", command));
            }
            response_body.result = help_text;
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "quit",
        help_text: "Disconnect (and suspend) the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, response_body, _, _| {
            target_core.core.halt(Duration::from_millis(500))?;
            response_body.result = "Debug Session Terminated".to_string();
            Ok(DebugSessionStatus::Terminate)
        },
    },
    ReplCommand {
        command: "c",
        help_text: "Continue running the program on the target.",
        sub_commands: None,
        args: None,
        handler: |target_core, response_body, _, _| {
            target_core.core.run()?;
            response_body.result = CoreStatus::Running.short_long_status(None).1;
            target_core.core_data.last_known_status = CoreStatus::Running;
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "break",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Sets a breakpoint specified location, or next instruction if unspecified.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Optional("*address")]),
        handler: |target_core, response_body, command_arguments, _| {
            if command_arguments.is_empty() {
                let core_info = target_core.core.halt(Duration::from_millis(500))?;
                response_body.result = CoreStatus::Halted(HaltReason::Request)
                    .short_long_status(Some(core_info.pc))
                    .1;
            } else {
                // TODO: Currently this sets breakpoints without synching the VSCode UI. We can send a Dap `breakpoint` event.
                println!("Setting breakpoint at address: {}", command_arguments);

                let mut input_arguments = command_arguments.split_whitespace();
                if let Some(input_argument) = input_arguments.next() {
                    if input_argument.starts_with("*0x") || input_argument.starts_with("*0X") {
                        if let Ok(memory_reference) = u64::from_str_radix(&input_argument[3..], 16)
                        {
                            target_core.set_breakpoint(
                                memory_reference,
                                BreakpointType::InstructionBreakpoint,
                            )?;
                            response_body.result =
                                format!("Added breakpoint @ {:#010x}", memory_reference);
                        } else {
                            return Err(DebuggerError::ReplError(
                                "Invalid hex address.".to_string(),
                            ));
                        }
                    } else {
                        return Err(DebuggerError::ReplError(
                            "Invalid parameters. See the `help` command for more information."
                                .to_string(),
                        ));
                    }
                }
            }
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "backtrace",
        sub_commands: None,
        help_text: "Print the backtrace of the current thread.",
        args: None,
        handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
    },
    ReplCommand {
        command: "info",
        help_text: "Information of specified program data.",
        sub_commands: Some(&[
            ReplCommand {
                command: "threads",
                help_text: "List all threads.",
                sub_commands: None,
                args: None,
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "frame",
                help_text:
                    "Describe the current frame, or the frame at the specified (hex) address.",
                sub_commands: None,
                args: Some(&[ReplCommandArgs::Optional("address")]),
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "locals",
                help_text: "List local variables of the selected frame.",
                sub_commands: None,
                args: None,
                handler: |target_core, response_body, _, evaluate_arguments| {
                    // Make sure we have a valid StackFrame
                    if let Some(stack_frame) = match evaluate_arguments.frame_id {
                        Some(frame_id) => target_core
                            .core_data
                            .stack_frames
                            .iter_mut()
                            .find(|stack_frame| stack_frame.id == frame_id),
                        None => {
                            // Use the current frame_id
                            target_core.core_data.stack_frames.first_mut()
                        }
                    } {
                        if let Some(variable_cache) = stack_frame.local_variables.as_mut() {
                            if let Some(local_variable_root) =
                                variable_cache.get_variable_by_name(&VariableName::LocalScopeRoot)
                            {
                                response_body.memory_reference =
                                    Some(format!("{}", local_variable_root.memory_location));
                                response_body.result =
                                    "Local Variables : Click to expand".to_string();
                                response_body.type_ =
                                    Some(format!("{:?}", local_variable_root.type_name));
                                response_body.variables_reference =
                                    local_variable_root.variable_key;
                                response_body.presentation_hint = Some(VariablePresentationHint {
                                    attributes: None,
                                    kind: None,
                                    lazy: Some(false),
                                    visibility: None,
                                });
                                Ok(DebugSessionStatus::Continue)
                            } else {
                                Err(DebuggerError::ReplError(format!(
                                    "No local variables found for frame: {:?}.",
                                    stack_frame.function_name
                                )))
                            }
                        } else {
                            Err(DebuggerError::ReplError(format!(
                                "No variables available for frame: {:?}.",
                                stack_frame.function_name
                            )))
                        }
                    } else {
                        Err(DebuggerError::ReplError(("No frame selected.").to_string()))
                    }
                },
            },
            ReplCommand {
                command: "all-reg",
                help_text: "List all registers of the selected frame.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
            ReplCommand {
                command: "var",
                help_text: "List all static variables.",
                sub_commands: None,
                // TODO: Add & implement arguments.
                args: None,
                handler: |_, _, _, _| Err(DebuggerError::Unimplemented),
            },
        ]),
        args: None,
        handler: |_, _, _, _| {
            Err(DebuggerError::ReplError("Please provide one of the required subcommands. See the `help` command for more information.".to_string()))
        },
    },
    ReplCommand {
        command: "p",
        // Stricly speaking, gdb refers to this as an expression, but we only support variables.
        help_text: "Print known information about variable.",
        sub_commands: None,
        args: Some(&[ReplCommandArgs::Required("<variable name>")]),
        handler: |target_core, response_body, variable_name, evaluate_arguments| {
            // Make sure we have a valid StackFrame
            if let Some(stack_frame) = match evaluate_arguments.frame_id {
                Some(frame_id) => target_core
                    .core_data
                    .stack_frames
                    .iter_mut()
                    .find(|stack_frame| stack_frame.id == frame_id),
                None => {
                    // Use the current frame_id
                    target_core.core_data.stack_frames.first_mut()
                }
            } {
                if let Some(variable_cache) = stack_frame.local_variables.as_mut() {
                    if let Some(variable) = variable_cache
                        .get_variable_by_name(&VariableName::Named(variable_name.to_string()))
                    {
                        response_body.memory_reference =
                            Some(format!("{}", variable.memory_location));
                        response_body.result = format!(
                            "{} : {} ",
                            variable.name,
                            variable.get_value(variable_cache)
                        );
                        response_body.type_ = Some(format!("{:?}", variable.type_name));
                        response_body.variables_reference = variable.variable_key;
                    } else {
                        return Err(DebuggerError::ReplError(format!(
                            "No variable named {:?} found for frame: {:?}.",
                            variable_name, stack_frame.function_name
                        )));
                    }
                } else {
                    return Err(DebuggerError::ReplError(format!(
                        "No variables available for frame: {:?}.",
                        stack_frame.function_name
                    )));
                }
            } else {
                return Err(DebuggerError::ReplError("No frame selected.".to_string()));
            }
            Ok(DebugSessionStatus::Continue)
        },
    },
    ReplCommand {
        command: "x",
        help_text: "Examine Memory, using format specifications, at the specified address.",
        sub_commands: None,
        args: Some(&[
            ReplCommandArgs::Optional("/Nuf (N=count, u=unit, f=format)"),
            ReplCommandArgs::Optional("address (hex)"),
        ]),
        handler: |target_core, response_body, command_arguments, request_arguments| {
            let input_arguments = command_arguments.split_whitespace();
            let mut gdb_nuf = GdbNuf {
                ..Default::default()
            };
            // Sequence of evaluations will be:
            // 1. Specified address
            // 2. Frame address
            // 3. Program counter
            let mut input_address = 0_u64;

            for input_argument in input_arguments {
                if input_argument.starts_with("0x") || input_argument.starts_with("0X") {
                    if let Ok(memory_reference) = u64::from_str_radix(&input_argument[2..], 16) {
                        input_address = memory_reference;
                    } else {
                        return Err(DebuggerError::ReplError("Invalid hex address.".to_string()));
                    }
                } else if input_argument.starts_with('/') {
                    if let Some(gdb_nuf_string) = input_argument.strip_prefix('/') {
                        gdb_nuf = GdbNuf::try_from(gdb_nuf_string)?;
                    } else {
                        return Err(DebuggerError::ReplError(
                            "The '/' specifier must be followed by a valid gdb 'Nuf' format specifier."
                                .to_string(),
                        ));
                    }
                } else {
                    return Err(DebuggerError::ReplError(
                        "Invalid parameters. See the `help` command for more information."
                            .to_string(),
                    ));
                }
            }
            if input_address == 0 {
                // No address was specified, so we'll use the frame address, if available.
                input_address = if let Some(frame_pc) = request_arguments
                    .frame_id
                    .and_then(|frame_id| {
                        target_core
                            .core_data
                            .stack_frames
                            .iter_mut()
                            .find(|stack_frame| stack_frame.id == frame_id)
                    })
                    .map(|stack_frame| stack_frame.pc)
                {
                    frame_pc.try_into()?
                } else {
                    target_core
                        .core
                        .read_core_reg(target_core.core.registers().program_counter())?
                }
            }

            memory_read(input_address, gdb_nuf, target_core, response_body)
        },
    },
];

/// Read memory at the specified address (hex), using the [`GdbNuf`] specifiers to determine size and format.
fn memory_read(
    address: u64,
    gdb_nuf: GdbNuf,
    target_core: &mut CoreHandle,
    response_body: &mut EvaluateResponseBody,
) -> Result<DebugSessionStatus, DebuggerError> {
    if gdb_nuf.format_specifier == GdbFormat::Instruction {
        let assembly_lines: Vec<DisassembledInstruction> = disassemble_target_memory(
            target_core,
            0_i64,
            0_i64,
            address,
            gdb_nuf.unit_count as i64,
        )?;
        if assembly_lines.is_empty() {
            return Err(DebuggerError::ReplError(format!(
                "Cannot disassemble memory at address {:#010x}",
                address
            )));
        } else {
            let mut formatted_output = "".to_string();
            for assembly_line in &assembly_lines {
                formatted_output.push_str(&assembly_line.to_string());
            }
            response_body.result = formatted_output;
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
                response_body.result = formatted_output;
            }
            Err(err) => {
                return Err(DebuggerError::ReplError(format!(
                    "Cannot read memory at address {:#010x}: {:?}",
                    address, err
                )))
            }
        }
    }
    Ok(DebugSessionStatus::Continue)
}

/// Get a list of command matches, based on the given command piece.
/// The `command_piece` is a valid [`ReplCommand`], which can be either a command or a sub_command.
fn find_commands<'a>(
    repl_commands: Vec<&'a ReplCommand<H>>,
    command_piece: &'a str,
) -> Vec<&'a ReplCommand<H>> {
    repl_commands
        .into_iter()
        .filter(move |command| command.command.starts_with(command_piece))
        .collect::<Vec<&ReplCommand<H>>>()
}

/// Iteratively builds a list of command matches, based on the given filter.
/// If multiple levels of commands are involved, the ReplCommand::command will be concatenated.
pub(crate) fn build_expanded_commands(command_filter: &str) -> (String, Vec<&ReplCommand<H>>) {
    // Split the given text into a command, optional sub-command, and optional arguments.
    let command_pieces = command_filter.split(&[' ', '/', '*'][..]);

    // Always start building from the top-level commands.
    let mut repl_commands: Vec<&ReplCommand<H>> = REPL_COMMANDS.iter().collect();

    let mut command_root = "".to_string();
    for command_piece in command_pieces {
        // Find the matching commands.
        let matches = find_commands(repl_commands.clone(), command_piece);

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
            REPL_COMMANDS.iter().collect::<Vec<&ReplCommand<H>>>(),
        )
    } else {
        // Iterate over the command pieces, and find the matching commands.
        let (command_root, command_list) = build_expanded_commands(&arguments.text);
        (format!("{} ", command_root), command_list)
    };
    command_list
        .iter()
        .map(|command| CompletionItem {
            // Add a space after the command, so that the user can start typing the next command.
            // This space will be trimmed if the user selects to evaluate the command as is.
            label: format!("{}{} ", command_root, command.command),
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
