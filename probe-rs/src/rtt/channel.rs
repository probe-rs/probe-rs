use crate::rtt::Error;
use crate::{config::MemoryRegion, Core, MemoryInterface};
use scroll::{Pread, LE};
use std::cmp::min;

/// Trait for channel information shared between up and down channels.
pub trait RttChannel {
    /// Returns the number of the channel.
    fn number(&self) -> usize;

    /// Returns the name of the channel or `None` if there is none.
    fn name(&self) -> Option<&str>;

    /// Returns the buffer size in bytes. Note that the usable size is one byte less due to how the
    /// ring buffer is implemented.
    fn buffer_size(&self) -> usize;
}

#[derive(Debug)]
pub(crate) struct Channel {
    number: usize,
    core_id: usize,
    ptr: u64,
    name: Option<String>,
    buffer_ptr: u64,
    size: u64,
    is_64bit: bool,
}

// Chanels must follow this data layout when reading/writing memory in order to be compatible with
// the official RTT implementation.
//
// struct Channel {
//     const char *name; // Name of channel, pointer to null-terminated string. Optional.
//     char *buffer; // Pointer to buffer data
//     unsigned int size; // Size of data buffer. The actual capacity is one byte less.
//     unsigned int write; // Offset in data buffer of next byte to write.
//     unsigned int read; // Offset in data buffer of next byte to read.
//     // The low 2 bits of flags are used for blocking/non blocking modes, the rest are ignored.
//     unsigned int flags;
// }

impl Channel {
    // Size of the Channel struct in target memory in bytes
    pub(crate) const SIZE: usize = 24;
    pub(crate) const SIZE_64: usize = 48;

    // Offsets of fields in target memory in bytes
    const O_NAME: usize = 0;
    const O_BUFFER_PTR: usize = 4;
    const O_SIZE: usize = 8;
    const O_WRITE: usize = 12;
    const O_READ: usize = 16;
    const O_FLAGS: usize = 20;
    const O_BUFFER_PTR_64: usize = 8;
    const O_SIZE_64: usize = 16;
    const O_WRITE_64: usize = 24;
    const O_READ_64: usize = 32;
    const O_FLAGS_64: usize = 40;

    pub(crate) fn from(
        core: &mut Core,
        number: usize,
        memory_map: &[MemoryRegion],
        ptr: u64,
        mem: &[u8],
        is_64bit: bool,
    ) -> Result<Option<Channel>, Error> {
        let buffer_ptr: u64 = match if is_64bit {
            let p: Result<u64, scroll::Error> = mem.pread_with(Self::O_BUFFER_PTR_64, LE);
            p
        } else {
            let p: Result<u32, scroll::Error> = mem.pread_with(Self::O_BUFFER_PTR, LE);
            p.map(|p32| u64::from(p32))
        } {
            Ok(p) => p,
            Err(_e) => return Err(super::Error::MemoryRead("RTT channel address".to_string())),
        };

        if buffer_ptr == 0 {
            // This buffer isn't in use
            return Ok(None);
        }

        // TODO ここの仕組みを直したい
        let name_ptr: u64 = match if is_64bit {
            let p: Result<u64, scroll::Error> = mem.pread_with(Self::O_NAME, LE);
            p
        } else {
            let p: Result<u32, scroll::Error> = mem.pread_with(Self::O_NAME, LE);
            p.map(|p32| u64::from(p32))
        } {
            Ok(p) => p,
            Err(_e) => return Err(super::Error::MemoryRead("RTT channel name".to_string())),
        };

        let name = if name_ptr == 0 {
            None
        } else {
            read_c_string(core, memory_map, name_ptr)?
        };

        let size: u64 = if is_64bit {
            mem.pread_with(Self::O_SIZE_64, LE).unwrap()
        } else {
            let s: u32 = mem.pread_with(Self::O_SIZE, LE).unwrap();
            s.into()
        };

        Ok(Some(Channel {
            number,
            core_id: core.id(),
            ptr,
            name,
            buffer_ptr,
            size,
            is_64bit,
        }))
    }

    /// Validate that the Core id of a request is the same as the Core id against which the Channel was created.
    pub(crate) fn validate_core_id(&self, core: &mut Core) -> Result<(), Error> {
        if core.id() == self.core_id {
            Ok(())
        } else {
            Err(Error::IncorrectCoreSpecified(self.core_id, core.id()))
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_ref().map(|s| s.as_ref())
    }

    pub fn buffer_size(&self) -> usize {
        self.size as usize
    }

    fn read_pointers(&self, core: &mut Core, dir: &'static str) -> Result<(u64, u64), Error> {
        self.validate_core_id(core)?;

        let (write, read): (u64, u64) = if self.is_64bit {
            let mut block = [0u64; 2];
            core.read_64((self.ptr + Self::O_WRITE_64 as u64).into(), block.as_mut())?;
            (block[0], block[1])
        } else {
            let mut block = [0u32; 2];
            core.read_32((self.ptr + Self::O_WRITE as u64).into(), block.as_mut())?;
            (u64::from(block[0]), u64::from(block[1]))
        };

        let validate = |which, value| {
            if value >= self.size {
                Err(Error::ControlBlockCorrupted(format!(
                    "{} pointer is {} while buffer size is {} for {:?} channel {} ({})",
                    which,
                    value,
                    self.size,
                    dir,
                    self.number,
                    self.name().unwrap_or("no name"),
                )))
            } else {
                Ok(())
            }
        };

        validate("write", write)?;
        validate("read", read)?;

        Ok((write, read))
    }
}

/// RTT up (target to host) channel.
#[derive(Debug)]
pub struct UpChannel(pub(crate) Channel);

impl UpChannel {
    /// Returns the number of the channel.
    pub fn number(&self) -> usize {
        self.0.number
    }

    /// Returns the name of the channel or `None` if there is none.
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }

    /// Returns the buffer size in bytes. Note that the usable size is one byte less due to how the
    /// ring buffer is implemented.
    pub fn buffer_size(&self) -> usize {
        self.0.buffer_size()
    }

    /// Reads the current channel mode from the target and returns its.
    ///
    /// See [`ChannelMode`] for more information on what the modes mean.
    pub fn mode(&self, core: &mut Core) -> Result<ChannelMode, Error> {
        self.0.validate_core_id(core)?;

        let flags = if self.0.is_64bit {
            core.read_word_64((self.0.ptr + Channel::O_FLAGS_64 as u64).into())?
        } else {
            u64::from(core.read_word_32((self.0.ptr + Channel::O_FLAGS as u64).into())?)
        };

        match flags & 0x3 {
            0 => Ok(ChannelMode::NoBlockSkip),
            1 => Ok(ChannelMode::NoBlockTrim),
            2 => Ok(ChannelMode::BlockIfFull),
            _ => Err(Error::ControlBlockCorrupted(String::from(
                "The channel mode flags are invalid",
            ))),
        }
    }

    /// Changes the channel mode on the target to the specified mode.
    ///
    /// See [`ChannelMode`] for more information on what the modes mean.
    pub fn set_mode(&self, core: &mut Core, mode: ChannelMode) -> Result<(), Error> {
        self.0.validate_core_id(core)?;
        let flags = if self.0.is_64bit {
            core.read_word_64((self.0.ptr + Channel::O_FLAGS_64 as u64).into())?
        } else {
            u64::from(core.read_word_32((self.0.ptr + Channel::O_FLAGS as u64).into())?)
        };

        let new_flags = (flags & !3) | (mode as u64);
        if self.0.is_64bit {
            core.write_word_64((self.0.ptr + Channel::O_FLAGS_64 as u64).into(), new_flags)?
        } else {
            core.write_word_32(
                (self.0.ptr + Channel::O_FLAGS as u64).into(),
                new_flags as u32,
            )?
        };

        Ok(())
    }

    fn read_core(&self, core: &mut Core, mut buf: &mut [u8]) -> Result<(u64, usize), Error> {
        self.0.validate_core_id(core)?;
        let (write, mut read) = self.0.read_pointers(core, "up")?;

        let mut total = 0;

        // Read while buffer contains data and output buffer has space (maximum of two iterations)
        while !buf.is_empty() {
            let count = min(self.readable_contiguous(write, read), buf.len());
            if count == 0 {
                break;
            }

            core.read((self.0.buffer_ptr + read).into(), &mut buf[..count])?;

            total += count;
            read += count as u64;

            if read >= self.0.size {
                // Wrap around to start
                read = 0;
            }

            buf = &mut buf[count..];
        }

        Ok((read, total))
    }

    /// Reads some bytes from the channel to the specified buffer and returns how many bytes were
    /// read.
    ///
    /// This method will not block waiting for data in the target buffer, and may read less bytes
    /// than would fit in `buf`.
    pub fn read(&self, core: &mut Core, buf: &mut [u8]) -> Result<usize, Error> {
        self.0.validate_core_id(core)?;
        let (read, total) = self.read_core(core, buf)?;

        if total > 0 {
            // Write read pointer back to target if something was read
            if self.0.is_64bit {
                core.write_word_64((self.0.ptr + Channel::O_READ_64 as u64).into(), read)?;
            } else {
                core.write_word_32((self.0.ptr + Channel::O_READ as u64).into(), read as u32)?;
            }
        }

        Ok(total)
    }

    /// Peeks at the current data in the channel buffer, copies data into the specified buffer and
    /// returns how many bytes were read.
    ///
    /// The difference from [`read`](UpChannel::read) is that this does not discard the data in the
    /// buffer.
    pub fn peek(&self, core: &mut Core, buf: &mut [u8]) -> Result<usize, Error> {
        self.0.validate_core_id(core)?;
        Ok(self.read_core(core, buf)?.1)
    }

    /// Calculates amount of contiguous data available for reading
    fn readable_contiguous(&self, write: u64, read: u64) -> usize {
        (if read > write {
            self.0.size - read
        } else {
            write - read
        }) as usize
    }
}

impl RttChannel for UpChannel {
    /// Returns the number of the channel.
    fn number(&self) -> usize {
        self.0.number
    }

    fn name(&self) -> Option<&str> {
        self.0.name()
    }
    fn buffer_size(&self) -> usize {
        self.0.buffer_size()
    }
}

/// RTT down (host to target) channel.
#[derive(Debug)]
pub struct DownChannel(pub(crate) Channel);

impl DownChannel {
    /// Returns the number of the channel.
    pub fn number(&self) -> usize {
        self.0.number
    }

    /// Returns the name of the channel or `None` if there is none.
    pub fn name(&self) -> Option<&str> {
        self.0.name()
    }

    /// Returns the buffer size in bytes. Note that the usable size is one byte less due to how the
    /// ring buffer is implemented.
    pub fn buffer_size(&self) -> usize {
        self.0.buffer_size()
    }

    /// Writes some bytes into the channel buffer and returns the number of bytes written.
    ///
    /// This method will not block waiting for space to become available in the channel buffer, and
    /// may not write all of `buf`.
    pub fn write(&self, core: &mut Core, mut buf: &[u8]) -> Result<usize, Error> {
        self.0.validate_core_id(core)?;
        let (mut write, read) = self.0.read_pointers(core, "down")?;

        if self.writable_contiguous(write, read) == 0 {
            // Buffer is full - do nothing.
            return Ok(0);
        }

        let mut total = 0;

        // Write while buffer has space for data and output contains data (maximum of two iterations)
        while !buf.is_empty() {
            let count = min(self.writable_contiguous(write, read), buf.len());
            if count == 0 {
                break;
            }

            core.write_8((self.0.buffer_ptr + write).into(), &buf[..count])?;

            total += count;
            write += count as u64;

            if write >= self.0.size {
                // Wrap around to start
                write = 0;
            }

            buf = &buf[count..];
        }

        // Write write pointer back to target
        if self.0.is_64bit {
            core.write_word_64(self.0.ptr + Channel::O_WRITE_64 as u64, write)?;
        } else {
            core.write_word_32(self.0.ptr + Channel::O_WRITE as u64, write as u32)?;
        }

        Ok(total)
    }

    /// Calculates amount of contiguous space available for writing
    fn writable_contiguous(&self, write: u64, read: u64) -> usize {
        (if read > write {
            read - write - 1
        } else if read == 0 {
            self.0.size - write - 1
        } else {
            self.0.size - write
        }) as usize
    }
}

impl RttChannel for DownChannel {
    /// Returns the number of the channel.
    fn number(&self) -> usize {
        self.0.number
    }

    fn name(&self) -> Option<&str> {
        self.0.name()
    }
    fn buffer_size(&self) -> usize {
        self.0.buffer_size()
    }
}

/// Reads a null-terminated string from target memory. Lossy UTF-8 decoding is used.
fn read_c_string(
    core: &mut Core,
    memory_map: &[MemoryRegion],
    ptr: u64,
) -> Result<Option<String>, Error> {
    // Find out which memory range contains the pointer
    let range = memory_map
        .iter()
        .filter_map(|r| match r {
            MemoryRegion::Nvm(r) => Some(&r.range),
            MemoryRegion::Ram(r) => Some(&r.range),
            _ => None,
        })
        .find(|r| r.contains(&ptr));

    // If the pointer is not within any valid range, return None.
    let range = match range {
        Some(r) => r,
        None => return Ok(None),
    };

    // Read up to 128 bytes not going past the end of the region
    let mut bytes = vec![0u8; min(128, (range.end - ptr as u64) as usize)];
    core.read(ptr, bytes.as_mut())?;

    let return_value = bytes
        .iter()
        .position(|&b| b == 0)
        .map(|p| String::from_utf8_lossy(&bytes[..p]).into_owned());
    tracing::debug!(
        "probe-rs-rtt::Channel::read_c_string() result = {:?}",
        return_value
    );
    // If the bytes read contain a null, return the preceding part as a string, otherwise None.
    Ok(return_value)
}

/// Specifies what to do when a channel doesn't have enough buffer space for a complete write on the
/// target side.
#[derive(Clone, Copy, Eq, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
#[repr(u32)]
pub enum ChannelMode {
    /// Skip writing the data completely if it doesn't fit in its entirety.
    NoBlockSkip = 0,

    /// Write as much as possible of the data and ignore the rest.
    NoBlockTrim = 1,

    /// Block (spin) if the buffer is full. Note that if the application writes within a critical
    /// section, using this mode can cause the application to freeze if the buffer becomes full and
    /// is not read by the host.
    BlockIfFull = 2,
}
