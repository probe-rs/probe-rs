//! ARM semihosting support.
//!
//! Specification: <https://github.com/ARM-software/abi-aa/blob/2024Q3/semihosting/semihosting.rst>

use std::{num::NonZeroU32, time::SystemTime};

use crate::{CoreInterface, Error, MemoryInterface, RegisterValue};

/// Indicates the operation the target would like the debugger to perform.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum SemihostingCommand {
    /// The target indicates that it completed successfully and no-longer wishes
    /// to run.
    ExitSuccess,

    /// The target indicates that it completed unsuccessfully, with an error
    /// code, and no-longer wishes to run.
    ExitError(ExitErrorDetails),

    /// The target indicates that it would like to read the command line arguments.
    GetCommandLine(GetCommandLineRequest),

    /// The target requests to open a file on the host.
    Open(OpenRequest),

    /// The target requests to close a file on the host.
    Close(CloseRequest),

    /// The target indicated that it would like to write to the console.
    WriteConsole(WriteConsoleRequest),

    /// The target indicated that it would like to write to the console.
    Write(WriteRequest),

    /// The target indicated that it would like to read from a file on the host.
    Read(ReadRequest),

    /// The target indicated that it would like to seek in a file on the host.
    Seek(SeekRequest),

    /// The target indicated that it would like to read the length of a file on the host.
    FileLength(FileLengthRequest),

    /// The target indicated that it would like to remove a file on the host.
    Remove(RemoveRequest),

    /// The target indicated that it would like to rename a file on the host.
    Rename(RenameRequest),

    /// The target indicated that it would like to read the current time.
    Time(TimeRequest),

    /// The target indicated that it would like to read the value of errno.
    Errno(ErrnoRequest),

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
            write!(f, ", exit_status: {exit_status}")?;
        }
        if let Some(subcode) = self.subcode {
            write!(f, ", subcode: {subcode:#x}")?;
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

/// A request to open a file on the host.
///
/// Note that this is not implemented by probe-rs yet.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct OpenRequest {
    path: ZeroTerminatedString,
    mode: &'static str,
}

impl OpenRequest {
    /// Reads the path from the target.
    pub fn path(&self, core: &mut dyn CoreInterface) -> Result<String, Error> {
        self.path.read(core)
    }

    /// Reads the raw mode from the target.
    pub fn mode(&self) -> &'static str {
        self.mode
    }

    /// Responds with the opened file handle to the target.
    pub fn respond_with_handle(
        &self,
        core: &mut dyn CoreInterface,
        handle: NonZeroU32,
    ) -> Result<(), Error> {
        write_status(core, handle.get() as i32)
    }
}

/// A request to open a file on the host.
///
/// Note that this is not implemented by probe-rs yet.
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct CloseRequest {
    handle: u32,
}

impl CloseRequest {
    /// Returns the handle of the file to close
    pub fn file_handle(&self) -> u32 {
        self.handle
    }

    /// Responds with success to the target.
    pub fn success(&self, core: &mut dyn CoreInterface) -> Result<(), Error> {
        write_status(core, 0)
    }
}

/// A request to write to the console
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WriteConsoleRequest(pub(crate) ZeroTerminatedString);
impl WriteConsoleRequest {
    /// Reads the string from the target
    pub fn read(&self, core: &mut crate::Core<'_>) -> Result<String, Error> {
        self.0.read(core)
    }
}

/// A request to write to the console
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct WriteRequest {
    handle: u32,
    bytes: u32,
    len: u32,
}
impl WriteRequest {
    /// Returns the handle of the file to write to
    pub fn file_handle(&self) -> u32 {
        self.handle
    }

    /// Reads the buffer from the target
    pub fn read(&self, core: &mut crate::Core<'_>) -> Result<Vec<u8>, Error> {
        let mut buf = vec![0u8; self.len as usize];
        core.read(self.bytes as u64, &mut buf)?;
        Ok(buf)
    }

    /// Writes the status of the semihosting operation to the return register of the target
    pub fn write_status(&self, core: &mut dyn CoreInterface, status: i32) -> Result<(), Error> {
        write_status(core, status)
    }
}

/// A request to read from a file on the host
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct ReadRequest {
    handle: u32,
    bytes: u32,
    len: u32,
}

impl ReadRequest {
    /// Returns the handle of the file to read from
    pub fn file_handle(&self) -> u32 {
        self.handle
    }

    /// Returns the number of bytes to read
    pub fn bytes_to_read(&self) -> u32 {
        self.len
    }

    /// Writes the buffer to the target
    pub fn write_buffer_to_target(
        &self,
        core: &mut crate::Core<'_>,
        buf: &[u8],
    ) -> Result<(), Error> {
        assert!(buf.len() <= self.len as usize);

        if !buf.is_empty() {
            core.write(self.bytes as u64, buf)?;
        }

        let status = match buf.len() {
            0 => self.len as i32,
            len if len == self.len as usize => 0,
            len => len as i32,
        };

        write_status(core, status)
    }
}

/// A request to seek in a file on the host
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct SeekRequest {
    handle: u32,
    pos: u32,
}

impl SeekRequest {
    /// Returns the handle of the file to seek in
    pub fn file_handle(&self) -> u32 {
        self.handle
    }

    /// Returns the absolute byte position to search to
    pub fn position(&self) -> u32 {
        self.pos
    }

    /// Responds with success to the target
    pub fn success(&self, core: &mut dyn CoreInterface) -> Result<(), Error> {
        write_status(core, 0)
    }
}

/// A request to read the length of a file on the host
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct FileLengthRequest {
    handle: u32,
}

impl FileLengthRequest {
    /// Returns the handle of the file to seek in
    pub fn file_handle(&self) -> u32 {
        self.handle
    }

    /// Writes the file length to the target
    pub fn write_length(&self, core: &mut dyn CoreInterface, len: i32) -> Result<(), Error> {
        write_status(core, len)
    }
}

/// A request to remove a file on the host
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct RemoveRequest {
    path: ZeroTerminatedString,
}

impl RemoveRequest {
    /// Reads the path from the target.
    pub fn path(&self, core: &mut dyn CoreInterface) -> Result<String, Error> {
        self.path.read(core)
    }

    /// Responds with success to the target
    pub fn success(&self, core: &mut dyn CoreInterface) -> Result<(), Error> {
        write_status(core, 0)
    }
}

/// A request to rename a file on the host
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct RenameRequest {
    from_path: ZeroTerminatedString,
    to_path: ZeroTerminatedString,
}

impl RenameRequest {
    /// Reads the path of the old file from the target.
    pub fn from_path(&self, core: &mut dyn CoreInterface) -> Result<String, Error> {
        self.from_path.read(core)
    }

    /// Reads the path for the new file from the target.
    pub fn to_path(&self, core: &mut dyn CoreInterface) -> Result<String, Error> {
        self.to_path.read(core)
    }

    /// Responds with success to the target
    pub fn success(&self, core: &mut dyn CoreInterface) -> Result<(), Error> {
        write_status(core, 0)
    }
}

/// A request to read the current time
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct TimeRequest {}
impl TimeRequest {
    /// Writes the time to the target
    pub fn write_time(&self, core: &mut dyn CoreInterface, value: u32) -> Result<(), Error> {
        write_status(core, value as i32)
    }

    /// Writes the current time to the target
    pub fn write_current_time(&self, core: &mut dyn CoreInterface) -> Result<(), Error> {
        let duration = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("Failed to get system time");
        self.write_time(core, duration.as_secs() as u32)
    }
}

/// A request to read the errno
#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub struct ErrnoRequest {}
impl ErrnoRequest {
    /// Writes the errno to the target
    pub fn write_errno(&self, core: &mut dyn CoreInterface, errno: i32) -> Result<(), Error> {
        // On exit, the RETURN REGISTER contains the value of the C library errno variable.
        write_status(core, errno)
    }
}

fn write_status(core: &mut dyn CoreInterface, value: i32) -> Result<(), crate::Error> {
    let reg = core.registers().get_argument_register(0).unwrap();
    core.write_core_reg(reg.into(), RegisterValue::U32(value as u32))?;

    Ok(())
}

/// When using some semihosting commands, the target usually allocates a buffer for the host to read/write to.
/// The targets just gives us an address pointing to two u32 values, the address of the buffer and
/// the length of the buffer.
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

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
pub(crate) struct ZeroTerminatedString {
    pub address: u32,
    pub length: Option<u32>,
}

impl ZeroTerminatedString {
    /// Reads the buffer contents from the target.
    pub fn read(&self, core: &mut dyn CoreInterface) -> Result<String, Error> {
        let mut bytes = Vec::new();

        if let Some(len) = self.length {
            bytes = vec![0; len as usize];
            core.read(self.address as u64, &mut bytes)?;
        } else {
            let mut buf = [0; 128];
            let mut from = self.address as u64;

            loop {
                core.read(from, &mut buf)?;
                if let Some(end) = buf.iter().position(|&x| x == 0) {
                    bytes.extend_from_slice(&buf[..end]);
                    break;
                }

                bytes.extend_from_slice(&buf);
                from += buf.len() as u64;
            }
        }

        Ok(String::from_utf8_lossy(&bytes).to_string())
    }
}

/// Decodes a semihosting syscall without running the requested action.
/// Only supports SYS_EXIT, SYS_EXIT_EXTENDED and SYS_GET_CMDLINE at the moment
pub fn decode_semihosting_syscall(
    core: &mut dyn CoreInterface,
) -> Result<SemihostingCommand, Error> {
    let operation: u32 = core
        .read_core_reg(core.registers().get_argument_register(0).unwrap().id())?
        .try_into()?;
    let parameter: u32 = core
        .read_core_reg(core.registers().get_argument_register(1).unwrap().id())?
        .try_into()?;

    tracing::debug!("Semihosting found r0={operation:#x} r1={parameter:#x}");

    // This is defined by the ARM Semihosting Specification:
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#semihosting-operations>

    const SYS_GET_CMDLINE: u32 = 0x15;
    const SYS_EXIT: u32 = 0x18;
    const SYS_EXIT_EXTENDED: u32 = 0x20;
    const SYS_EXIT_ADP_STOPPED_APPLICATIONEXIT: u32 = 0x20026;
    const SYS_OPEN: u32 = 0x01;
    const SYS_CLOSE: u32 = 0x02;
    const SYS_WRITEC: u32 = 0x03;
    const SYS_WRITE0: u32 = 0x04;
    const SYS_WRITE: u32 = 0x05;
    const SYS_READ: u32 = 0x06;
    const SYS_SEEK: u32 = 0x0a;
    const SYS_FLEN: u32 = 0x0c;
    const SYS_REMOVE: u32 = 0x0e;
    const SYS_RENAME: u32 = 0x0f;
    const SYS_TIME: u32 = 0x11;
    const SYS_ERRNO: u32 = 0x13;

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

        (SYS_OPEN, pointer) => {
            let [string, mode, str_len] = param(core, pointer)?;

            // signal to target: status = failure, in case the application does not answer this request
            // -1 is the error value for SYS_OPEN
            write_status(core, -1)?;
            SemihostingCommand::Open(OpenRequest {
                path: ZeroTerminatedString {
                    address: string,
                    length: Some(str_len),
                },
                mode: match mode {
                    0 => "r",
                    1 => "rb",
                    2 => "r+",
                    3 => "r+b",
                    4 => "w",
                    5 => "wb",
                    6 => "w+",
                    7 => "w+b",
                    8 => "a",
                    9 => "ab",
                    10 => "a+",
                    11 => "a+b",
                    _ => "unknown",
                },
            })
        }

        (SYS_CLOSE, pointer) => {
            let [handle] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            // -1 is the error value for SYS_CLOSE
            write_status(core, -1)?;
            SemihostingCommand::Close(CloseRequest { handle })
        }

        (SYS_WRITEC, pointer) => {
            SemihostingCommand::WriteConsole(WriteConsoleRequest(ZeroTerminatedString {
                address: pointer,
                length: Some(1),
            }))
            // no response is given
        }

        (SYS_WRITE0, pointer) => {
            SemihostingCommand::WriteConsole(WriteConsoleRequest(ZeroTerminatedString {
                address: pointer,
                length: None,
            }))
            // no response is given
        }

        (SYS_WRITE, pointer) => {
            let [handle, bytes, len] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::Write(WriteRequest { handle, bytes, len })
        }

        (SYS_READ, pointer) => {
            let [handle, bytes, len] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::Read(ReadRequest { handle, bytes, len })
        }

        (SYS_SEEK, pointer) => {
            let [handle, pos] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::Seek(SeekRequest { handle, pos })
        }

        (SYS_FLEN, pointer) => {
            let [handle] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::FileLength(FileLengthRequest { handle })
        }

        (SYS_REMOVE, pointer) => {
            let [path, len] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::Remove(RemoveRequest {
                path: ZeroTerminatedString {
                    address: path,
                    length: Some(len),
                },
            })
        }

        (SYS_RENAME, pointer) => {
            let [from_path, from_len, to_path, to_len] = param(core, pointer)?;
            // signal to target: status = failure, in case the application does not answer this request
            write_status(core, -1)?;
            SemihostingCommand::Rename(RenameRequest {
                from_path: ZeroTerminatedString {
                    address: from_path,
                    length: Some(from_len),
                },
                to_path: ZeroTerminatedString {
                    address: to_path,
                    length: Some(to_len),
                },
            })
        }

        (SYS_TIME, 0) => SemihostingCommand::Time(TimeRequest {}),

        (SYS_ERRNO, 0) => SemihostingCommand::Errno(ErrnoRequest {}),

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

fn param<const N: usize>(
    core: &mut dyn CoreInterface,
    pointer: u32,
) -> Result<[u32; N], crate::Error> {
    let mut buf = [0; N];
    core.read_32(pointer as u64, &mut buf)?;
    Ok(buf)
}
