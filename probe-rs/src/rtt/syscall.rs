use crate::SemihostingCommand;

/// Decode a semihosting syscall. Only SYS_EXIT is supported at the moment
pub fn decode_semihosting_syscall(operation: u32, parameter: u32) -> SemihostingCommand {
    // This is defined by the ARM Semihosting Specification:
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#semihosting-operations>
    const SYS_EXIT: u32 = 0x18;
    const SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT: u32 = 0x20026;
    //const SYS_GET_CMDLINE: u32 = 0x15;
    match (operation, parameter) {
        (SYS_EXIT, SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT) => SemihostingCommand::ExitSuccess,
        (SYS_EXIT, code) => SemihostingCommand::ExitError { code: code as u64 },
        /*(SYS_GET_CMDLINE, block_address) => {
            let mut block : [u32; 2] = [0, 0];
            core.read_32(block_address as u64, &mut block).unwrap();
            tracing::error!("SYS_GET_CMDLINE block addr {:#x}", block_address);
            SemihostingCommand::Unknown {
                operation,
                parameter,
            }
        }*/
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
