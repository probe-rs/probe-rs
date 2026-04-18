use crate::cmd::dap_server::{
    DebuggerError,
    debug_adapter::{
        dap::{
            dap_types::{/*DisassembleResponseBody,*/ EvaluateArguments, MemoryAddress},
            repl_commands::{DebugAdapter, EvalResponse, EvalResult, REPL_COMMANDS, ReplCommand},
            repl_types::ReplCommandArgs,
        },
        protocol::ProtocolAdapter,
    },
    server::core_data::CoreHandle,
};
use linkme::distributed_slice;

#[distributed_slice(REPL_COMMANDS)]
static DISASSEMBLE: ReplCommand = ReplCommand {
    command: "disassemble",
    help_text: "Disassembles the specified instruction count, beginning at the specified address.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[
        ReplCommandArgs::Required("start"),
        ReplCommandArgs::Required("instructions"),
    ],
    handler: disassemble,
};

fn disassemble(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    adapter: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    // Current limitations:
    //
    // - No support for disassembling a function by its global name, or by a
    // local variable that points to it
    //   - Global lookups require either a .symtab or debug_info
    //   - Local lookups require a debug_info and some idea of the current
    //   stack frame index; the latter also requires "up" and "down" commands
    // - No support for doing math on the address or instruction-count
    //   - Would require parsing an expression, and potentially dereferencing
    //   many symbol or register values, then doing the offset
    // - An instruction count is always required
    //   - When function lookup support is added we could fetch the size as well
    //   as the address, and go to the end

    let mut input_arguments = command_arguments.split_whitespace();
    let Some(address_str) = input_arguments.next() else {
        return Err(DebuggerError::UserMessage(format!(
            "Invalid parameters {command_arguments:?} (bad address). See the `help` command for more information."
        )));
    };
    let Some(instructions_str) = input_arguments.next() else {
        return Err(DebuggerError::UserMessage(format!(
            "Invalid parameters {command_arguments:?} (bad instruction count). See the `help` command for more information."
        )));
    };

    let address = if let Some(reg) = address_str.strip_prefix('$') {
        let Some(register) = target_core.core.registers().all_registers().find(|r| {
            std::iter::once(r.name().to_string())
                .chain(r.roles.iter().map(|role| role.to_string()))
                .any(|name| name.eq_ignore_ascii_case(reg))
        }) else {
            return Err(DebuggerError::UserMessage(format!(
                "Invalid parameter {command_arguments:?}: invalid register."
            )));
        };
        target_core.core.read_core_reg(register)?
    } else if let Ok(mem_addr) = MemoryAddress::try_from(address_str.trim_start_matches('*')) {
        mem_addr.0
    } else {
        // Try to resolve a global symbol
        return Err(DebuggerError::Unimplemented);
    } as i64;

    let instructions: i64 = parse_int::parse(instructions_str).map_err(|error| {
        DebuggerError::UserMessage(format!(
            "Invalid instruction count: {instructions_str:?}: {error:?}"
        ))
    })?;

    let instructions = adapter.get_disassembled_source(target_core, address, 0, 0, instructions)?;

    Ok(EvalResponse::Message(
        instructions
            .iter()
            .map(|insn| insn.to_string())
            .collect::<Vec<_>>()
            .join(""),
    ))
}
