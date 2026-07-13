use linkme::distributed_slice;
use probe_rs::{CoreRegister, RegisterRole, RegisterValue};

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

//  `wreg` command: "Set the value of a core or peripheral register."
#[distributed_slice(REPL_COMMANDS)]
static WREG: ReplCommand = ReplCommand {
    command: "wreg",
    help_text: "Write a value to the named register in the selected core, e.g. `wreg pc 0x1000`.",
    requires_target_halted: true,
    sub_commands: &[],
    args: &[
        ReplCommandArgs::Required("register name"),
        ReplCommandArgs::Required("value"),
    ],
    handler: write_register,
};

/// Split the `wreg` arguments into a register name and the value to write.
///
/// The value accepts the same decimal/hexadecimal/octal/binary notation as the other CLI commands.
fn parse_wreg_args(command_arguments: &str) -> Result<(&str, u128), DebuggerError> {
    let mut parts = command_arguments.split_whitespace();

    let Some(register_name) = parts.next() else {
        return Err(DebuggerError::UserMessage(
            "Please specify a register name and a value, e.g. `wreg pc 0x1000`.".to_string(),
        ));
    };

    let Some(value) = parts.next() else {
        return Err(DebuggerError::UserMessage(format!(
            "Please specify a value to write to register {register_name:?}, e.g. `wreg {register_name} 0x1000`."
        )));
    };

    if parts.next().is_some() {
        return Err(DebuggerError::UserMessage(
            "Too many arguments. Usage: `wreg <register name> <value>`.".to_string(),
        ));
    }

    let value = parse_int::parse::<u128>(value)
        .map_err(|error| DebuggerError::UserMessage(format!("Invalid value {value:?}: {error}")))?;

    Ok((register_name, value))
}

/// Match a register by any of its role names (case-insensitive).
///
/// Registers may be addressed by architectural names like `R15`, or by role aliases like `PC`,
/// `SP`, `LR`, `FP`, and `MSP`, matching how `info reg` displays them. `sp` also matches the main
/// stack pointer, since some architectures only mark it with the `MainStackPointer` role.
fn register_matches(register: &CoreRegister, query: &str) -> bool {
    register
        .roles
        .iter()
        .any(|role| role.to_string().eq_ignore_ascii_case(query))
        || (query.eq_ignore_ascii_case("sp")
            && register.register_has_role(RegisterRole::MainStackPointer))
}

fn write_register(
    target_core: &mut CoreHandle<'_>,
    command_arguments: &str,
    _: &EvaluateArguments,
    _: &mut DebugAdapter<dyn ProtocolAdapter + '_>,
) -> EvalResult {
    let (register_name, value) = parse_wreg_args(command_arguments)?;

    let register = target_core
        .core
        .registers()
        .all_registers()
        .find(|reg| register_matches(reg, register_name))
        .ok_or_else(|| {
            DebuggerError::UserMessage(format!(
                "No register found matching {register_name:?}. Use `info reg` to list the available registers."
            ))
        })?;

    // Build a `RegisterValue` that matches the width of the target register, and reject
    // values that would not fit.
    let size_in_bits = register.size_in_bits();
    let register_value = if size_in_bits <= 32 {
        if value > u32::MAX as u128 {
            return Err(too_large(value, size_in_bits, register.name()));
        }
        RegisterValue::U32(value as u32)
    } else if size_in_bits <= 64 {
        if value > u64::MAX as u128 {
            return Err(too_large(value, size_in_bits, register.name()));
        }
        RegisterValue::U64(value as u64)
    } else {
        RegisterValue::U128(value)
    };

    let id = register.id();
    let name = register.name();

    target_core.core.write_core_reg(id, register_value)?;

    // Read the value back so the user can confirm the write took effect.
    let read_back = target_core.core.read_core_reg::<RegisterValue>(id)?;

    Ok(EvalResponse::Message(format!("{name}: {read_back}")))
}

fn too_large(value: u128, size_in_bits: usize, register_name: &str) -> DebuggerError {
    DebuggerError::UserMessage(format!(
        "Value {value:#x} does not fit into the {size_in_bits}-bit register {register_name}."
    ))
}

#[cfg(test)]
mod test {
    use super::parse_wreg_args;

    #[test]
    fn parses_name_and_hex_value() {
        assert!(matches!(parse_wreg_args("pc 0x1000"), Ok(("pc", 0x1000))));
    }

    #[test]
    fn parses_decimal_value() {
        assert!(matches!(parse_wreg_args("sp 4096"), Ok(("sp", 4096))));
    }

    #[test]
    fn tolerates_extra_whitespace() {
        assert!(matches!(
            parse_wreg_args("  r0   0b1010  "),
            Ok(("r0", 0b1010))
        ));
    }

    #[test]
    fn missing_value_is_an_error() {
        assert!(parse_wreg_args("pc").is_err());
    }

    #[test]
    fn missing_arguments_is_an_error() {
        assert!(parse_wreg_args("").is_err());
    }

    #[test]
    fn too_many_arguments_is_an_error() {
        assert!(parse_wreg_args("pc 0x10 0x20").is_err());
    }

    #[test]
    fn invalid_value_is_an_error() {
        assert!(parse_wreg_args("pc notanumber").is_err());
    }
}
