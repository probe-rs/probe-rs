use crate::SemihostingCommand;

/// Decode a semihosting syscall. Only SYS_EXIT is supported at the moment
pub fn decode_semihosting_syscall(operation: u32, parameter: u32) -> SemihostingCommand {
    // This is defined by the ARM Semihosting Specification:
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#semihosting-operations>
    const SYS_EXIT: u32 = 0x18;
    const SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT: u32 = 0x20026;
    match (operation, parameter) {
        (SYS_EXIT, SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT) => SemihostingCommand::ExitSuccess,
        (SYS_EXIT, code) => SemihostingCommand::ExitError { code: code as u64 },
        _ => {
            tracing::warn!(
                "Unknown semihosting operation={operation:04x} parameter={parameter:04x}"
            );
            SemihostingCommand::Unknown {
                operation,
                parameter,
            }
        }
    }
}
