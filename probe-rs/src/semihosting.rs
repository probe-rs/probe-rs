use crate::{CoreInterface, Error, RegisterValue};

/// Indicates the operation the target would like the debugger to perform.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub enum SemihostingCommand {
    /// The target indicates that it completed successfully and no-longer wishes
    /// to run.
    ExitSuccess,

    /// The target indicates that it completed unsuccessfully, with an error
    /// code, and no-longer wishes to run.
    ExitError(ExitErrorDetails),

    /// The target indicates that it would like to read the command line arguments.
    GetCommandLine(GetCommandLineRequest),

    /// The target indicated that it would like to run a semihosting operation which we don't support yet.
    Unknown(UnknownCommandDetails),
}

/// Details of a semihosting exit with error
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct ExitErrorDetails {
    /// Some application specific exit reason:
    /// <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#651entry-32-bit>
    pub reason: u32,

    /// The exit status of the application, if present (only if reason == `ADP_Stopped_ApplicationExit` `0x20026`).
    /// This is an exit status code, as passed to the C standard library exit() function.
    pub exit_status: Option<u32>,

    /// The subcode of the exit, if present (only if reason != `ADP_Stopped_ApplicationExit` `0x20026`).
    pub subcode: Option<u32>,
}

impl std::fmt::Display for ExitErrorDetails {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "reason: {:#x}", self.reason)?;
        if let Some(exit_status) = self.exit_status {
            write!(f, ", exit_status: {}", exit_status)?;
        }
        if let Some(subcode) = self.subcode {
            write!(f, ", subcode: {:#x}", subcode)?;
        }
        Ok(())
    }
}

/// Details of a semihosting operation that we don't support yet
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct UnknownCommandDetails {
    /// The semihosting operation requested
    pub operation: u32,

    /// The parameter to the semihosting operation
    pub parameter: u32,
}

impl UnknownCommandDetails {
    /// Returns the buffer pointed-to by the parameter of the semihosting operation
    pub fn get_buffer(&self, core: &mut dyn CoreInterface) -> Result<Buffer, Error> {
        Buffer::from_block_at(core, self.parameter)
    }

    /// Writes the status of the semihosting operation to the return register of the target
    pub fn write_status(&self, core: &mut dyn CoreInterface, status: i32) -> Result<(), Error> {
        write_status(core, status)
    }
}

/// A request to read the command line arguments from the target
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct GetCommandLineRequest(Buffer);

impl GetCommandLineRequest {
    /// Writes the command line to the target. You have to continue the core manually afterwards.
    pub fn write_command_line_to_target(
        &self,
        core: &mut dyn CoreInterface,
        cmdline: &str,
    ) -> Result<(), Error> {
        let mut buf = cmdline.to_owned().into_bytes();
        buf.push(0);
        self.0.write(core, &buf)?;

        // signal to target: status = success
        write_status(core, 0)?;

        Ok(())
    }
}

fn write_status(core: &mut dyn CoreInterface, value: i32) -> Result<(), crate::Error> {
    let reg = core.registers().get_argument_register(0).unwrap();
    core.write_core_reg(reg.into(), RegisterValue::U32(value as u32))?;

    Ok(())
}

// When using some semihosting commands, the target usually allocates a buffer for the host to read/write to.
// The targets just gives us an address pointing to two u32 values, the address of the buffer and
// the length of the buffer.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct Buffer {
    buffer_location: u32, // The address where the buffer address and length are stored
    address: u32,         // The start of the buffer
    len: u32,             // The length of the buffer
}

impl Buffer {
    /// Constructs a new buffer, reading the address and length from the target.
    pub fn from_block_at(
        core: &mut dyn CoreInterface,
        block_addr: u32,
    ) -> Result<Self, crate::Error> {
        let mut block: [u32; 2] = [0, 0];
        core.read_32(block_addr as u64, &mut block)?;
        Ok(Self {
            buffer_location: block_addr,
            address: block[0],
            len: block[1],
        })
    }

    /// Reads the buffer contents from the target.
    pub fn read(&self, core: &mut dyn CoreInterface) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; self.len as usize];
        core.read(self.address as u64, &mut buf[..])?;
        Ok(buf)
    }

    /// Writes the passed buffer to the target buffer.
    /// The buffer must end with \0. Length written to target will not include \0.
    pub fn write(&self, core: &mut dyn CoreInterface, buf: &[u8]) -> Result<(), Error> {
        if buf.len() > self.len as usize {
            return Err(Error::Other("buffer not large enough".to_string()));
        }
        if buf.last() != Some(&0) {
            return Err(Error::Other("last byte of buffer must be 0".to_string()));
        }
        core.write_8(self.address as u64, buf)?;
        let block: [u32; 2] = [self.address, (buf.len() - 1) as u32];
        core.write_32(self.buffer_location as u64, &block)?;
        Ok(())
    }
}

/// Decodes a semihosting syscall without running the requested action.
/// Only supports SYS_EXIT, SYS_EXIT_EXTENDED and SYS_GET_CMDLINE at the moment
pub fn decode_semihosting_syscall(
    core: &mut dyn CoreInterface,
    operation: u32,
    parameter: u32,
) -> Result<SemihostingCommand, Error> {
    // This is defined by the ARM Semihosting Specification:
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#semihosting-operations>

    const SYS_GET_CMDLINE: u32 = 0x15;
    const SYS_EXIT: u32 = 0x18;
    const SYS_EXIT_EXTENDED: u32 = 0x20;
    const SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT: u32 = 0x20026;
    Ok(match (operation, parameter) {
        (SYS_EXIT, SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT) => SemihostingCommand::ExitSuccess,
        (SYS_EXIT, reason) => SemihostingCommand::ExitError(ExitErrorDetails {
            reason,
            exit_status: None,
            subcode: None,
        }),

        (SYS_EXIT_EXTENDED, block_address) => {
            // Parameter points to a block of memory containing two 32-bit words.
            let mut buf = [0u32; 2];
            core.read_32(block_address as u64, &mut buf)?;
            let reason = buf[0];
            let subcode = buf[1];
            match (reason, subcode) {
                (SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT, 0) => SemihostingCommand::ExitSuccess,
                (SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT, exit_status) => {
                    SemihostingCommand::ExitError(ExitErrorDetails {
                        reason,
                        exit_status: Some(exit_status),
                        subcode: None,
                    })
                }
                (reason, subcode) => SemihostingCommand::ExitError(ExitErrorDetails {
                    reason,
                    exit_status: None,
                    subcode: Some(subcode),
                }),
            }
        }

        (SYS_GET_CMDLINE, block_address) => {
            // signal to target: status = failure, in case the application does not answer this request
            // -1 is the error value for SYS_GET_CMDLINE
            write_status(core, -1)?;
            SemihostingCommand::GetCommandLine(GetCommandLineRequest(Buffer::from_block_at(
                core,
                block_address,
            )?))
        }
        _ => {
            // signal to target: status = failure, in case the application does not answer this request
            // It is not guaranteed that a value of -1 will be treated as an error by the target, but it is a common value to indicate an error.
            write_status(core, -1)?;

            tracing::debug!(
                "Unknown semihosting operation={operation:04x} parameter={parameter:04x}"
            );
            SemihostingCommand::Unknown(UnknownCommandDetails {
                operation,
                parameter,
            })
        }
    })
}
