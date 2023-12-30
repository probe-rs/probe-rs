use crate::{CoreInterface, Error, SemihostingCommand};

/// Decode a semihosting syscall. Only SYS_EXIT is supported at the moment
pub fn decode_semihosting_syscall(
    core: &mut dyn CoreInterface,
    operation: u32,
    parameter: u32,
) -> Result<SemihostingCommand, Error> {
    // This is defined by the ARM Semihosting Specification:
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#semihosting-operations>
    const SYS_EXIT: u32 = 0x18;
    const SYS_EXIT_EXTENDED: u32 = 0x20;
    const SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT: u32 = 0x20026;
    Ok(match (operation, parameter) {
        (SYS_EXIT, SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT) => SemihostingCommand::ExitSuccess,
        (SYS_EXIT, reason) => SemihostingCommand::ExitError {
            reason,
            exit_status: None,
            subcode: None,
        },
        (SYS_EXIT_EXTENDED, block_address) => {
            // Parameter points to a block of memory containing two 32-bit words.
            let mut buf = [0u32; 2];
            core.read_32(block_address as u64, &mut buf)?;
            let reason = buf[0];
            let subcode = buf[1];
            match (reason, subcode) {
                (SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT, 0) => SemihostingCommand::ExitSuccess,
                (SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT, exit_status) => {
                    SemihostingCommand::ExitError {
                        reason,
                        exit_status: Some(exit_status),
                        subcode: None,
                    }
                }
                (reason, subcode) => SemihostingCommand::ExitError {
                    reason,
                    exit_status: None,
                    subcode: Some(subcode),
                },
            }
        }
        _ => {
            tracing::warn!(
                "Unknown semihosting operation={operation:04x} parameter={parameter:04x}"
            );
            SemihostingCommand::Unknown {
                operation,
                parameter,
            }
        }
    })
}
