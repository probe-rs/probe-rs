//! Host side implementation of the RTT (Real-Time Transfer) I/O protocol over probe-rs
//!
//! RTT implements input and output to/from a microcontroller using in-memory ring buffers and
//! memory polling. This enables debug logging from the microcontroller with minimal delays and no
//! blocking, making it usable even in real-time applications where e.g. semihosting delays cannot
//! be tolerated.
//!
//! This crate enables you to read and write via RTT channels. It's also used as a building-block
//! for probe-rs debugging tools.
//!
//! ## Example
//!
//! ```no_run
//! use probe_rs::probe::list::Lister;
//! use probe_rs::Permissions;
//! use probe_rs::rtt::Rtt;
//!
//! // First obtain a probe-rs session (see probe-rs documentation for details)
//! let lister = Lister::new();
//!
//! let probes = lister.list_all();
//!
//! let probe = probes[0].open()?;
//! let mut session = probe.attach("somechip", Permissions::default())?;
//! let memory_map = session.target().memory_map.clone();
//! // Select a core.
//! let mut core = session.core(0)?;
//!
//! // Attach to RTT
//! let mut rtt = Rtt::attach(&mut core, &memory_map)?;
//!
//! // Read from a channel
//! if let Some(input) = rtt.up_channels().take(0) {
//!     let mut buf = [0u8; 1024];
//!     let count = input.read(&mut core, &mut buf[..])?;
//!
//!     println!("Read data: {:?}", &buf[..count]);
//! }
//!
//! // Write to a channel
//! if let Some(output) = rtt.down_channels().take(0) {
//!     output.write(&mut core, b"Hello, computer!\n")?;
//! }
//!
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod channel;
pub use channel::*;

pub mod channels;
pub use channels::Channels;

use crate::{config::MemoryRegion, Core, MemoryInterface};
use std::borrow::Cow;
use std::ops::Range;
use zerocopy::FromBytes;
use zerocopy_derive::{FromBytes, FromZeroes};

/// The RTT interface.
///
/// Use [`Rtt::attach`] or [`Rtt::attach_region`] to attach to a probe-rs [`Core`] and detect the channels, as they were
///     configured on the target.
/// The timing of when this is called is really important, or else unexpected results can be expected.
///
/// ## Examples of how timing between host and target effects the results
///
/// 1. **Scenario: Ideal configuration** The host RTT interface is created AFTER the target program has successfully executing the RTT
/// initialization, by calling an api such as [rtt:target](https://github.com/mvirkkunen/rtt-target)`::rtt_init_print!()`
///     * At this point, both the RTT Control Block and the RTT Channel configurations are present in the target memory, and
/// this RTT interface can be expected to work as expected.
///
/// 2. **Scenario: Failure to detect RTT Control Block** The target has been configured correctly, BUT the host creates this interface BEFORE
/// the target program has initialized RTT.
///     * This most commonly occurs when the target halts processing before initializing RTT. For example, this could happen ...
///         * During debugging, if the user sets a breakpoint in the code before the RTT initialization.
///         * After flashing, if the user has configured `probe-rs` to `reset_after_flashing` AND `halt_after_reset`. On most targets, this
/// will result in the target halting with reason `Exception` and will delay the subsequent RTT initialization.
///         * If RTT initialization on the target is delayed because of time consuming processing or excessive interrupt handling. This can
/// usually be prevented by moving the RTT initialization code to the very beginning of the target program logic.
///     * The result of such a timing issue is that `probe-rs` will fail to initialize RTT with an [`probe-rs-rtt::Error::ControlBlockNotFound`]
///
/// 3. **Scenario: Incorrect Channel names and incorrect Channel buffer sizes** This scenario usually occurs when two conditions coincide. Firstly, the same timing mismatch as described in point #2 above, and secondly, the target memory has NOT been cleared since a previous version of the binary program has been flashed to the target.
///     * What happens here is that the RTT Control Block is validated by reading a previously initialized RTT ID from the target memory. The next step in the logic is then to read the Channel configuration from the RTT Control block which is usually contains unreliable data
/// at this point. The symptoms will appear as:
///         * RTT Channel names are incorrect and/or contain unprintable characters.
///         * RTT Channel names are correct, but no data, or corrupted data, will be reported from RTT, because the buffer sizes are incorrect.
#[derive(Debug)]
pub struct Rtt {
    ptr: u64,

    /// The detected up (target to host) channels.
    pub up_channels: Channels<UpChannel>,

    /// The detected down (host to target) channels.
    pub down_channels: Channels<DownChannel>,
}

#[repr(C)]
#[derive(FromZeroes, FromBytes)]
struct RttControlBlockHeaderInner<T> {
    id: [u8; 16],
    max_up_channels: T,
    max_down_channels: T,
}

impl From<RttControlBlockHeaderInner<u32>> for RttControlBlockHeaderInner<u64> {
    fn from(value: RttControlBlockHeaderInner<u32>) -> Self {
        Self {
            id: value.id,
            max_up_channels: u64::from(value.max_up_channels),
            max_down_channels: u64::from(value.max_down_channels),
        }
    }
}

enum RttControlBlockHeader {
    Header32(RttControlBlockHeaderInner<u32>),
    Header64(RttControlBlockHeaderInner<u64>),
}

impl RttControlBlockHeader {
    pub fn try_from_header(is_64_bit: bool, mem: &[u8]) -> Option<Self> {
        if is_64_bit {
            RttControlBlockHeaderInner::<u64>::read_from(mem).map(Self::Header64)
        } else {
            RttControlBlockHeaderInner::<u32>::read_from(mem).map(Self::Header32)
        }
    }

    pub fn minimal_header_size(is_64_bit: bool) -> usize {
        if is_64_bit {
            std::mem::size_of::<RttControlBlockHeaderInner<u64>>()
        } else {
            std::mem::size_of::<RttControlBlockHeaderInner<u32>>()
        }
    }

    pub fn header_size(&self) -> usize {
        Self::minimal_header_size(matches!(self, Self::Header64(_)))
    }

    pub fn id(&self) -> [u8; 16] {
        match self {
            RttControlBlockHeader::Header32(x) => x.id,
            RttControlBlockHeader::Header64(x) => x.id,
        }
    }

    pub fn max_up_channels(&self) -> usize {
        match self {
            RttControlBlockHeader::Header32(x) => x.max_up_channels as usize,
            RttControlBlockHeader::Header64(x) => x.max_up_channels as usize,
        }
    }

    pub fn max_down_channels(&self) -> usize {
        match self {
            RttControlBlockHeader::Header32(x) => x.max_down_channels as usize,
            RttControlBlockHeader::Header64(x) => x.max_down_channels as usize,
        }
    }

    pub fn channel_buffer_size(&self) -> usize {
        match self {
            RttControlBlockHeader::Header32(_x) => RttChannelBufferInner::<u32>::size(),
            RttControlBlockHeader::Header64(_x) => RttChannelBufferInner::<u64>::size(),
        }
    }

    pub fn total_rtt_buffer_size(&self) -> usize {
        let total_number_of_channels = self.max_up_channels() + self.max_down_channels();
        let channel_size = self.channel_buffer_size();

        self.header_size() + channel_size * total_number_of_channels
    }

    pub fn parse_channel_buffers(&self, mem: &[u8]) -> Result<Vec<RttChannelBuffer>, Error> {
        let buffers = match self {
            RttControlBlockHeader::Header32(_) => RttChannelBufferInner::<u32>::slice_from(mem)
                .ok_or(Error::ControlBlockNotFound)?
                .iter()
                .copied()
                .map(RttChannelBuffer::from)
                .collect::<Vec<RttChannelBuffer>>(),
            RttControlBlockHeader::Header64(_) => RttChannelBufferInner::<u64>::slice_from(mem)
                .ok_or(Error::ControlBlockNotFound)?
                .iter()
                .copied()
                .map(RttChannelBuffer::from)
                .collect::<Vec<RttChannelBuffer>>(),
        };

        Ok(buffers)
    }
}

// Rtt must follow this data layout when reading/writing memory in order to be compatible with the
// official RTT implementation.
//
// struct ControlBlock {
//     char id[16]; // Used to find/validate the control block.
//     // Maximum number of up (target to host) channels in following array
//     unsigned int max_up_channels;
//     // Maximum number of down (host to target) channels in following array.
//     unsigned int max_down_channels;
//     RttChannel up_channels[max_up_channels]; // Array of up (target to host) channels.
//     RttChannel down_channels[max_down_channels]; // array of down (host to target) channels.
// }
impl Rtt {
    const RTT_ID: [u8; 16] = *b"SEGGER RTT\0\0\0\0\0\0";

    fn from(
        core: &mut Core,
        memory_map: &[MemoryRegion],
        // Pointer from which to scan
        ptr: u64,
        // Memory contents read in advance, starting from ptr
        mem_in: Option<&[u8]>,
    ) -> Result<Option<Rtt>, Error> {
        let is_64_bit = core.is_64_bit();

        let mut mem = match mem_in {
            Some(mem) => Cow::Borrowed(mem),
            None => {
                // If memory wasn't passed in, read the minimum header size
                let new_length = RttControlBlockHeader::minimal_header_size(is_64_bit);
                let mut mem = vec![0; new_length];
                core.read(ptr, &mut mem)?;
                Cow::Owned(mem)
            }
        };

        let rtt_header = RttControlBlockHeader::try_from_header(is_64_bit, &mem)
            .ok_or(Error::ControlBlockNotFound)?;

        // Validate that the control block starts with the ID bytes
        let rtt_id = rtt_header.id();
        if rtt_id != Self::RTT_ID {
            tracing::trace!(
                "Expected control block to start with RTT ID: {:?}\n. Got instead: {:?}",
                String::from_utf8_lossy(&Self::RTT_ID),
                String::from_utf8_lossy(&rtt_id)
            );
            return Err(Error::ControlBlockNotFound);
        }

        let max_up_channels = rtt_header.max_up_channels();
        let max_down_channels = rtt_header.max_down_channels();

        // *Very* conservative sanity check, most people only use a handful of RTT channels
        if max_up_channels > 255 || max_down_channels > 255 {
            return Err(Error::ControlBlockCorrupted(format!(
                "Unexpected array sizes at {ptr:#010x}: max_up_channels={max_up_channels} max_down_channels={max_down_channels}"
            )));
        }

        let cb_len = rtt_header.total_rtt_buffer_size();

        if let Cow::Owned(mem) = &mut mem {
            // If memory wasn't passed in, read the rest of the control block
            mem.resize(cb_len, 0);
            core.read(
                ptr + rtt_header.header_size() as u64,
                &mut mem[rtt_header.header_size()..cb_len],
            )?;
        }

        // Validate that the entire control block fits within the region
        if mem.len() < cb_len {
            tracing::debug!("Control block doesn't fit in scanned memory region.");
            return Ok(None);
        }

        let mut up_channels = Channels::new();
        let mut down_channels = Channels::new();

        let channel_buffer_size = rtt_header.channel_buffer_size();

        let up_channels_start = rtt_header.header_size();
        let up_channels_len = max_up_channels * channel_buffer_size;
        let up_channels_raw_buffer = &mem[up_channels_start..][..up_channels_len];
        let up_channels_buffer = rtt_header.parse_channel_buffers(up_channels_raw_buffer)?;

        let down_channels_start = up_channels_start + up_channels_len;
        let down_channels_len = max_down_channels * channel_buffer_size;
        let down_channels_raw_buffer = &mem[down_channels_start..][..down_channels_len];
        let down_channels_buffer = rtt_header.parse_channel_buffers(down_channels_raw_buffer)?;

        let mut offset = up_channels_start as u64;
        for (i, b) in up_channels_buffer.into_iter().enumerate() {
            if let Some(chan) = Channel::from(core, i, memory_map, ptr + offset, b)? {
                up_channels.push(UpChannel(chan));
            } else {
                tracing::warn!("Buffer for up channel {i} not initialized");
            }
            offset += b.size() as u64;
        }

        for (i, b) in down_channels_buffer.into_iter().enumerate() {
            if let Some(chan) = Channel::from(core, i, memory_map, ptr + offset, b)? {
                down_channels.push(DownChannel(chan));
            } else {
                tracing::warn!("Buffer for down channel {i} not initialized");
            }
            offset += b.size() as u64;
        }

        Ok(Some(Rtt {
            ptr,
            up_channels,
            down_channels,
        }))
    }

    /// Attempts to detect an RTT control block anywhere in the target RAM and returns an instance
    /// if a valid control block was found.
    ///
    /// `core` can be e.g. an owned `Core` or a shared `Rc<Core>`.
    pub fn attach(core: &mut Core, memory_map: &[MemoryRegion]) -> Result<Rtt, Error> {
        Self::attach_region(core, memory_map, &Default::default())
    }

    /// Attempts to detect an RTT control block in the specified RAM region(s) and returns an
    /// instance if a valid control block was found.
    ///
    /// `core` can be e.g. an owned `Core` or a shared `Rc<Core>`.
    pub fn attach_region(
        core: &mut Core,
        memory_map: &[MemoryRegion],
        region: &ScanRegion,
    ) -> Result<Rtt, Error> {
        let is_64_bit = core.is_64_bit();
        let ranges = match region.clone() {
            ScanRegion::Exact(addr) => {
                tracing::debug!("Scanning at exact address: {:#010x}", addr);

                return Rtt::from(core, memory_map, addr, None)?.ok_or(Error::ControlBlockNotFound);
            }
            ScanRegion::Ram => {
                tracing::debug!("Scanning whole RAM");

                memory_map
                    .iter()
                    .filter_map(MemoryRegion::as_ram_region)
                    .map(|r| r.range.clone())
                    .collect()
            }
            ScanRegion::Ranges(regions) => {
                tracing::debug!("Scanning regions: {:#010x?}", region);
                regions
            }

            ScanRegion::Range(region) => {
                tracing::debug!("Scanning region: {:#010x?}", region);
                vec![region]
            }
        };

        let minimal_header_size = RttControlBlockHeader::minimal_header_size(is_64_bit) as u64;
        let mut instances = ranges
            .into_iter()
            .filter_map(|range| {
                let range_len = match range.end.checked_sub(range.start) {
                    Some(v) if v < minimal_header_size => return None,
                    Some(v) => v,
                    None => return None,
                };

                let Ok(range_len) = usize::try_from(range_len) else {
                    // FIXME: This is not ideal because it means that we
                    // won't consider a >4GiB region if probe-rs is running
                    // on a 32-bit host, but it would be relatively unusual
                    // to use a 32-bit host to debug a 64-bit target.
                    tracing::warn!("Region too long ({} bytes), ignoring", range_len);
                    return None;
                };

                let mut mem = vec![0; range_len];
                core.read(range.start, &mut mem).ok()?;

                let offset = mem
                    .windows(Self::RTT_ID.len())
                    .position(|w| w == Self::RTT_ID)?;

                let target_ptr = range.start + offset as u64;

                Rtt::from(core, memory_map, target_ptr, Some(&mem[offset..])).transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;

        match instances.len() {
            0 => Err(Error::ControlBlockNotFound),
            1 => Ok(instances.remove(0)),
            _ => Err(Error::MultipleControlBlocksFound(instances)),
        }
    }

    /// Returns the memory address of the control block in target memory.
    pub fn ptr(&self) -> u64 {
        self.ptr
    }

    /// Gets a mutable reference to the detected up channels.
    pub fn up_channels(&mut self) -> &mut Channels<UpChannel> {
        &mut self.up_channels
    }

    /// Gets a mutable reference to the detected down channels.
    pub fn down_channels(&mut self) -> &mut Channels<DownChannel> {
        &mut self.down_channels
    }
}

/// Used to specify which memory regions to scan for the RTT control block.
#[derive(Clone, Debug, Default)]
pub enum ScanRegion {
    /// Scans all RAM regions known to probe-rs. This is the default and should always work, however
    /// if your device has a lot of RAM, scanning all of it is slow.
    #[default]
    Ram,

    /// Limit scanning to these memory addresses in target memory. It is up to the user to ensure
    /// that reading from this range will not read from undefined memory.
    ///
    /// This variant is equivalent to using [`Self::Ranges`] with a single range as long as the
    /// memory region fits into a 32-bit address space. This variant is for backward compatibility
    /// for code written before the addition of [`Self::Ranges`].
    Range(Range<u64>),

    /// Limit scanning to the memory addresses covered by all of the given ranges. It is up to the
    /// user to ensure that reading from this range will not read from undefined memory.
    Ranges(Vec<Range<u64>>),

    /// Tries to find the control block starting at this exact address. It is up to the user to
    /// ensure that reading the necessary bytes after the pointer will no read from undefined
    /// memory.
    Exact(u64),
}

/// Error type for RTT operations.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum Error {
    /// RTT control block not found in target memory.
    /// - Make sure RTT is initialized on the target, AND that there are NO target breakpoints before RTT initialization.
    /// - For VSCode and probe-rs-debugger users, using `halt_after_reset:true` in your `launch.json` file will prevent RTT
    ///   initialization from happening on time.
    /// - Depending on the target, sleep modes can interfere with RTT.
    ControlBlockNotFound,

    /// Multiple control blocks found in target memory: {display_list(_0)}.
    MultipleControlBlocksFound(Vec<Rtt>),

    /// The control block has been corrupted. {0}
    ControlBlockCorrupted(String),

    /// Attempted an RTT operation against a Core number that is different from the Core number against which RTT was initialized. Expected {0}, found {1}
    IncorrectCoreSpecified(usize, usize),

    /// Error communicating with probe: {0}
    Probe(#[from] crate::Error),

    /// Unexpected error while reading {0} from target memory. Please report this as a bug.
    MemoryRead(String),
}

fn display_list(list: &[Rtt]) -> String {
    list.iter()
        .map(|rtt| format!("{:#010x}", rtt.ptr))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_how_control_block_list_looks() {
        fn rtt(ptr: u32) -> Rtt {
            Rtt {
                ptr: ptr.into(),
                up_channels: Channels::new(),
                down_channels: Channels::new(),
            }
        }

        let error = Error::MultipleControlBlocksFound(vec![rtt(0x2000), rtt(0x3000)]);
        assert_eq!(
            error.to_string(),
            "Multiple control blocks found in target memory: 0x00002000, 0x00003000."
        );
    }
}
