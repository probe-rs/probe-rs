use crate::rtt::Error;
use crate::{Core, MemoryInterface};
use std::cmp::min;
use std::ffi::CStr;
use zerocopy::{FromBytes, Immutable, KnownLayout};

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

#[repr(C)]
#[derive(Debug, FromBytes, Immutable, KnownLayout, Clone)]
pub(crate) struct RttChannelBufferInner<T> {
    standard_name_pointer: T,
    buffer_start_pointer: T,
    size_of_buffer: T,
    write_offset: T,
    read_offset: T,
    flags: T,
}

impl<T> RttChannelBufferInner<T> {
    pub fn write_buffer_ptr_offset(&self) -> usize {
        std::mem::offset_of!(RttChannelBufferInner<T>, write_offset)
    }

    pub fn read_buffer_ptr_offset(&self) -> usize {
        std::mem::offset_of!(RttChannelBufferInner<T>, read_offset)
    }

    pub fn flags_offset(&self) -> usize {
        std::mem::offset_of!(RttChannelBufferInner<T>, flags)
    }

    pub fn size() -> usize {
        std::mem::size_of::<RttChannelBufferInner<T>>()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum RttChannelBuffer {
    Buffer32(RttChannelBufferInner<u32>),
    Buffer64(RttChannelBufferInner<u64>),
}

impl RttChannelBuffer {
    pub fn size(&self) -> usize {
        match self {
            RttChannelBuffer::Buffer32(_) => RttChannelBufferInner::<u32>::size(),
            RttChannelBuffer::Buffer64(_) => RttChannelBufferInner::<u64>::size(),
        }
    }
}

impl From<RttChannelBufferInner<u32>> for RttChannelBuffer {
    fn from(value: RttChannelBufferInner<u32>) -> Self {
        RttChannelBuffer::Buffer32(value)
    }
}

impl From<RttChannelBufferInner<u64>> for RttChannelBuffer {
    fn from(value: RttChannelBufferInner<u64>) -> Self {
        RttChannelBuffer::Buffer64(value)
    }
}

impl RttChannelBuffer {
    pub fn buffer_start_pointer(&self) -> u64 {
        match self {
            RttChannelBuffer::Buffer32(x) => u64::from(x.buffer_start_pointer),
            RttChannelBuffer::Buffer64(x) => x.buffer_start_pointer,
        }
    }

    pub fn standard_name_pointer(&self) -> u64 {
        match self {
            RttChannelBuffer::Buffer32(x) => u64::from(x.standard_name_pointer),
            RttChannelBuffer::Buffer64(x) => x.standard_name_pointer,
        }
    }

    pub fn size_of_buffer(&self) -> u64 {
        match self {
            RttChannelBuffer::Buffer32(x) => u64::from(x.size_of_buffer),
            RttChannelBuffer::Buffer64(x) => x.size_of_buffer,
        }
    }

    /// return (write_buffer_ptr, read_buffer_ptr)
    pub fn read_buffer_offsets(&self, core: &mut Core, ptr: u64) -> Result<(u64, u64), Error> {
        Ok(match self {
            RttChannelBuffer::Buffer32(h32) => {
                let mut block = [0u32; 2];
                core.read_32(ptr + h32.write_buffer_ptr_offset() as u64, block.as_mut())?;
                (u64::from(block[0]), u64::from(block[1]))
            }
            RttChannelBuffer::Buffer64(h64) => {
                let mut block = [0u64; 2];
                core.read_64(ptr + h64.write_buffer_ptr_offset() as u64, block.as_mut())?;
                (block[0], block[1])
            }
        })
    }

    pub fn write_write_buffer_ptr(
        &self,
        core: &mut Core,
        ptr: u64,
        buffer_ptr: u64,
    ) -> Result<(), Error> {
        match self {
            RttChannelBuffer::Buffer32(h32) => {
                core.write_word_32(
                    ptr + h32.write_buffer_ptr_offset() as u64,
                    buffer_ptr.try_into().unwrap(),
                )?;
            }
            RttChannelBuffer::Buffer64(h64) => {
                core.write_word_64(ptr + h64.write_buffer_ptr_offset() as u64, buffer_ptr)?;
            }
        };
        Ok(())
    }

    pub fn write_read_buffer_ptr(
        &self,
        core: &mut Core,
        ptr: u64,
        buffer_ptr: u64,
    ) -> Result<(), Error> {
        match self {
            RttChannelBuffer::Buffer32(h32) => {
                core.write_word_32(
                    ptr + h32.read_buffer_ptr_offset() as u64,
                    buffer_ptr.try_into().unwrap(),
                )?;
            }
            RttChannelBuffer::Buffer64(h64) => {
                core.write_word_64(ptr + h64.read_buffer_ptr_offset() as u64, buffer_ptr)?;
            }
        };
        Ok(())
    }

    pub fn read_flags(&self, core: &mut Core, ptr: u64) -> Result<u64, Error> {
        Ok(match self {
            RttChannelBuffer::Buffer32(h32) => {
                u64::from(core.read_word_32(ptr + h32.flags_offset() as u64)?)
            }
            RttChannelBuffer::Buffer64(h64) => {
                core.read_word_64(ptr + h64.flags_offset() as u64)?
            }
        })
    }

    pub fn write_flags(&self, core: &mut Core, ptr: u64, flags: u64) -> Result<(), Error> {
        match self {
            RttChannelBuffer::Buffer32(h32) => {
                core.write_word_32(ptr + h32.flags_offset() as u64, flags.try_into().unwrap())?;
            }
            RttChannelBuffer::Buffer64(h64) => {
                core.write_word_64(ptr + h64.flags_offset() as u64, flags)?;
            }
        };
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct Channel {
    number: usize,
    core_id: usize,
    name: Option<String>,
    metadata_ptr: u64,
    info: RttChannelBuffer,
    last_read_ptr: Option<u64>,
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
    pub(crate) fn from(
        core: &mut Core,
        number: usize,
        metadata_ptr: u64,
        info: RttChannelBuffer,
    ) -> Result<Option<Channel>, Error> {
        let buffer_ptr = info.buffer_start_pointer();
        if buffer_ptr == 0 {
            // This buffer isn't in use
            return Ok(None);
        };

        let this = Channel {
            number,
            core_id: core.id(),
            metadata_ptr,
            name: read_c_string(core, info.standard_name_pointer())?,
            info,
            last_read_ptr: None,
        };

        // It's possible that the channel is not initialized with the magic string written last.
        // We call read_pointers to validate that the channel pointers are in an expected range.
        // This should at least catch most cases where the control block is partially initialized.
        this.read_pointers(core, "")?;
        this.mode(core)?;

        Ok(Some(this))
    }

    /// Validate that the Core id of a request is the same as the Core id against which the Channel was created.
    pub(crate) fn validate_core_id(&self, core: &mut Core) -> Result<(), Error> {
        if core.id() != self.core_id {
            return Err(Error::IncorrectCoreSpecified(self.core_id, core.id()));
        }

        Ok(())
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn buffer_size(&self) -> usize {
        self.info.size_of_buffer() as usize
    }

    /// Reads the current channel mode from the target and returns its.
    ///
    /// See [`ChannelMode`] for more information on what the modes mean.
    pub fn mode(&self, core: &mut Core) -> Result<ChannelMode, Error> {
        self.validate_core_id(core)?;
        let flags = self.info.read_flags(core, self.metadata_ptr)?;

        ChannelMode::try_from(flags)
    }

    /// Changes the channel mode on the target to the specified mode.
    ///
    /// See [`ChannelMode`] for more information on what the modes mean.
    pub fn set_mode(&self, core: &mut Core, mode: ChannelMode) -> Result<(), Error> {
        self.validate_core_id(core)?;
        let flags = self.info.read_flags(core, self.metadata_ptr)?;

        let new_flags = ChannelMode::set(mode, flags);
        self.info.write_flags(core, self.metadata_ptr, new_flags)?;

        Ok(())
    }

    fn read_pointers(&self, core: &mut Core, channel_kind: &str) -> Result<(u64, u64), Error> {
        self.validate_core_id(core)?;

        let (write, read) = self.info.read_buffer_offsets(core, self.metadata_ptr)?;

        let validate = |which, value| {
            if value >= self.info.size_of_buffer() {
                Err(Error::ControlBlockCorrupted(format!(
                    "{which} pointer is {value} while buffer size is {} for {channel_kind}channel {} ({})",
                    self.info.size_of_buffer(),
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
        self.0.mode(core)
    }

    /// Changes the channel mode on the target to the specified mode.
    ///
    /// See [`ChannelMode`] for more information on what the modes mean.
    pub fn set_mode(&self, core: &mut Core, mode: ChannelMode) -> Result<(), Error> {
        self.0.set_mode(core, mode)
    }

    fn read_core(&mut self, core: &mut Core, mut buf: &mut [u8]) -> Result<(u64, usize), Error> {
        let (write, mut read) = self.0.read_pointers(core, "up ")?;

        let mut total = 0;

        if let Some(ptr) = self.0.last_read_ptr {
            // Check if the read pointer has changed since we last wrote it.
            if read != ptr {
                return Err(Error::ReadPointerChanged);
            }
        }

        // Read while buffer contains data and output buffer has space (maximum of two iterations)
        while !buf.is_empty() {
            let count = min(self.readable_contiguous(write, read), buf.len());
            if count == 0 {
                break;
            }

            core.read(self.0.info.buffer_start_pointer() + read, &mut buf[..count])?;

            total += count;
            read += count as u64;

            if read >= self.0.info.size_of_buffer() {
                // Wrap around to start
                read = 0;
            }

            buf = &mut buf[count..];
        }
        self.0.last_read_ptr = Some(read);

        Ok((read, total))
    }

    /// Reads some bytes from the channel to the specified buffer and returns how many bytes were
    /// read.
    ///
    /// This method will not block waiting for data in the target buffer, and may read less bytes
    /// than would fit in `buf`.
    pub fn read(&mut self, core: &mut Core, buf: &mut [u8]) -> Result<usize, Error> {
        let (read, total) = self.read_core(core, buf)?;

        if total > 0 {
            // Write read pointer back to target if something was read
            self.0
                .info
                .write_read_buffer_ptr(core, self.0.metadata_ptr, read)?;
        }

        Ok(total)
    }

    /// Peeks at the current data in the channel buffer, copies data into the specified buffer and
    /// returns how many bytes were read.
    ///
    /// The difference from [`read`](UpChannel::read) is that this does not discard the data in the
    /// buffer.
    pub fn peek(&mut self, core: &mut Core, buf: &mut [u8]) -> Result<usize, Error> {
        Ok(self.read_core(core, buf)?.1)
    }

    /// Calculates amount of contiguous data available for reading
    fn readable_contiguous(&self, write: u64, read: u64) -> usize {
        let end = if read > write {
            self.0.info.size_of_buffer()
        } else {
            write
        };

        (end - read) as usize
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
    pub fn write(&mut self, core: &mut Core, mut buf: &[u8]) -> Result<usize, Error> {
        let (mut write, read) = self.0.read_pointers(core, "down ")?;

        let mut total = 0;

        // Write while buffer has space for data and output contains data (maximum of two iterations)
        while !buf.is_empty() {
            let count = min(self.writable_contiguous(write, read), buf.len());
            if count == 0 {
                break;
            }

            core.write(self.0.info.buffer_start_pointer() + write, &buf[..count])?;

            total += count;
            write += count as u64;

            if write >= self.0.info.size_of_buffer() {
                // Wrap around to start
                write = 0;
            }

            buf = &buf[count..];
        }

        // Write write pointer back to target
        self.0
            .info
            .write_write_buffer_ptr(core, self.0.metadata_ptr, write)?;

        Ok(total)
    }

    /// Calculates amount of contiguous space available for writing
    fn writable_contiguous(&self, write: u64, read: u64) -> usize {
        (if read > write {
            read - write - 1
        } else if read == 0 {
            self.0.info.size_of_buffer() - write - 1
        } else {
            self.0.info.size_of_buffer() - write
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
fn read_c_string(core: &mut Core, ptr: u64) -> Result<Option<String>, Error> {
    // Find out which memory range contains the pointer
    if ptr == 0 {
        // If the pointer is null, return None.
        return Ok(None);
    }

    let Some(range) = core
        .memory_regions()
        .filter(|r| r.is_ram() || r.is_nvm())
        .find_map(|r| r.contains(ptr).then_some(r.address_range()))
    else {
        // If the pointer is not within any valid range, return None.
        tracing::warn!("RTT channel name points to unrecognized memory. Bad target description?");
        return Ok(None);
    };

    // Read up to 128 bytes not going past the end of the region
    let mut bytes = vec![0u8; min(128, (range.end - ptr) as usize)];
    core.read(ptr, bytes.as_mut())?;

    // If the bytes read contain a null, return the preceding part as a string, otherwise None.
    let return_value = CStr::from_bytes_until_nul(&bytes)
        .map(|s| s.to_string_lossy().into_owned())
        .ok();

    tracing::trace!("read_c_string() result = {:?}", return_value);
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

impl ChannelMode {
    fn set(self, flags: u64) -> u64 {
        (flags & !3) | (self as u64)
    }
}

impl TryFrom<u64> for ChannelMode {
    type Error = Error;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(ChannelMode::NoBlockSkip),
            1 => Ok(ChannelMode::NoBlockTrim),
            2 => Ok(ChannelMode::BlockIfFull),
            _ => Err(Error::ControlBlockCorrupted(format!(
                "The channel mode flags are invalid: {}",
                value
            ))),
        }
    }
}
