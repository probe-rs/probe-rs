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
//! // Select a core.
//! let mut core = session.core(0)?;
//!
//! // Attach to RTT
//! let mut rtt = Rtt::attach(&mut core)?;
//!
//! // Read from a channel
//! if let Some(input) = rtt.up_channel(0) {
//!     let mut buf = [0u8; 1024];
//!     let count = input.read(&mut core, &mut buf[..])?;
//!
//!     println!("Read data: {:?}", &buf[..count]);
//! }
//!
//! // Write to a channel
//! if let Some(output) = rtt.down_channel(0) {
//!     output.write(&mut core, b"Hello, computer!\n")?;
//! }
//!
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod channel;
pub use channel::*;

use crate::Session;
use crate::{Core, MemoryInterface, config::MemoryRegion};
use std::ops::Range;
use std::thread;
use std::time::Duration;
use std::time::Instant;
use zerocopy::{FromBytes, IntoBytes};

/// The RTT interface.
///
/// Use [`Rtt::attach`] or [`Rtt::attach_region`] to attach to a probe-rs [`Core`] and detect the
///     channels, as they were configured on the target. The timing of when this is called is really
///     important, or else unexpected results can be expected.
///
/// ## Examples of how timing between host and target effects the results
///
/// 1. **Scenario: Ideal configuration**: The host RTT interface is created **AFTER** the target
///    program has successfully executing the RTT initialization, by calling an api such as
///    [`rtt_target::rtt_init_print!()`](https://docs.rs/rtt-target/0.5.0/rtt_target/macro.rtt_init_print.html).
///
///    At this point, both the RTT Control Block and the RTT Channel configurations are present in
///    the target memory, and this RTT interface can be expected to work as expected.
///
/// 2. **Scenario: Failure to detect RTT Control Block**: The target has been configured correctly,
///    **BUT** the host creates this interface **BEFORE** the target program has initialized RTT.
///
///    This most commonly occurs when the target halts processing before initializing RTT. For
///    example, this could happen ...
///       * During debugging, if the user sets a breakpoint in the code before the RTT
///         initialization.
///       * After flashing, if the user has configured `probe-rs` to `reset_after_flashing` AND
///         `halt_after_reset`. On most targets, this will result in the target halting with
///         reason `Exception` and will delay the subsequent RTT initialization.
///       * If RTT initialization on the target is delayed because of time consuming processing or
///         excessive interrupt handling. This can usually be prevented by moving the RTT
///         initialization code to the very beginning of the target program logic.
///
///     The result of such a timing issue is that `probe-rs` will fail to initialize RTT with an
///    [`Error::ControlBlockNotFound`]
///
/// 3. **Scenario: Incorrect Channel names and incorrect Channel buffer sizes**: This scenario
///    usually occurs when two conditions coincide. Firstly, the same timing mismatch as described
///    in point #2 above, and secondly, the target memory has NOT been cleared since a previous
///    version of the binary program has been flashed to the target.
///
///    What happens here is that the RTT Control Block is validated by reading a previously
///    initialized RTT ID from the target memory. The next step in the logic is then to read the
///    Channel configuration from the RTT Control block which is usually contains unreliable data
///    at this point. The symptoms will appear as:
///       * RTT Channel names are incorrect and/or contain unprintable characters.
///       * RTT Channel names are correct, but no data, or corrupted data, will be reported from
///         RTT, because the buffer sizes are incorrect.
#[derive(Debug)]
pub struct Rtt {
    /// The location of the control block in target memory.
    ptr: u64,

    /// The detected up (target to host) channels.
    pub up_channels: Vec<UpChannel>,

    /// The detected down (host to target) channels.
    pub down_channels: Vec<DownChannel>,
}

#[repr(C)]
#[derive(FromBytes)]
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
            RttControlBlockHeaderInner::<u64>::read_from_prefix(mem)
                .map(|(header, _)| Self::Header64(header))
                .ok()
        } else {
            RttControlBlockHeaderInner::<u32>::read_from_prefix(mem)
                .map(|(header, _)| Self::Header32(header))
                .ok()
        }
    }

    pub const fn minimal_header_size(is_64_bit: bool) -> usize {
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
            RttControlBlockHeader::Header32(_) => {
                <[RttChannelBufferInner<u32>]>::ref_from_bytes(mem)
                    .map_err(|_| Error::ControlBlockNotFound)?
                    .iter()
                    .cloned()
                    .map(RttChannelBuffer::from)
                    .collect::<Vec<RttChannelBuffer>>()
            }
            RttControlBlockHeader::Header64(_) => {
                <[RttChannelBufferInner<u64>]>::ref_from_bytes(mem)
                    .map_err(|_| Error::ControlBlockNotFound)?
                    .iter()
                    .cloned()
                    .map(RttChannelBuffer::from)
                    .collect::<Vec<RttChannelBuffer>>()
            }
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
    /// The magic string expected to be found at the beginning of the RTT control block.
    pub const RTT_ID: [u8; 16] = *b"SEGGER RTT\0\0\0\0\0\0";

    /// Tries to attach to an RTT control block at the specified memory address.
    pub fn attach_at(
        core: &mut Core,
        // Pointer from which to scan
        ptr: u64,
    ) -> Result<Rtt, Error> {
        let is_64_bit = core.is_64_bit();

        let mut mem = [0u32; RttControlBlockHeader::minimal_header_size(true) / 4];
        // Read the magic value first as unordered data, and read the subsequent pointers
        // as ordered u32 values.
        core.read(ptr, &mut mem.as_mut_bytes()[0..Self::RTT_ID.len()])?;
        core.read_32(
            ptr + Self::RTT_ID.len() as u64,
            &mut mem[Self::RTT_ID.len() / 4..],
        )?;

        let rtt_header = RttControlBlockHeader::try_from_header(is_64_bit, mem.as_bytes())
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

        // Read the rest of the control block
        let channel_buffer_len = rtt_header.total_rtt_buffer_size() - rtt_header.header_size();
        let mut mem = vec![0; channel_buffer_len / 4];
        core.read_32(ptr + rtt_header.header_size() as u64, &mut mem)?;

        let mut up_channels = Vec::new();
        let mut down_channels = Vec::new();

        let channel_buffer_size = rtt_header.channel_buffer_size();

        let up_channels_start = 0;
        let up_channels_len = max_up_channels * channel_buffer_size;
        let up_channels_raw_buffer = &mem.as_bytes()[up_channels_start..][..up_channels_len];
        let up_channels_buffer = rtt_header.parse_channel_buffers(up_channels_raw_buffer)?;

        let down_channels_start = up_channels_start + up_channels_len;
        let down_channels_len = max_down_channels * channel_buffer_size;
        let down_channels_raw_buffer = &mem.as_bytes()[down_channels_start..][..down_channels_len];
        let down_channels_buffer = rtt_header.parse_channel_buffers(down_channels_raw_buffer)?;

        let mut offset = ptr + rtt_header.header_size() as u64 + up_channels_start as u64;
        for (channel_index, buffer) in up_channels_buffer.into_iter().enumerate() {
            let buffer_size = buffer.size() as u64;

            if let Some(chan) = Channel::from(core, channel_index, offset, buffer)? {
                up_channels.push(UpChannel(chan));
            } else {
                tracing::warn!("Buffer for up channel {channel_index} not initialized");
            }
            offset += buffer_size;
        }

        let mut offset = ptr + rtt_header.header_size() as u64 + down_channels_start as u64;
        for (channel_index, buffer) in down_channels_buffer.into_iter().enumerate() {
            let buffer_size = buffer.size() as u64;

            if let Some(chan) = Channel::from(core, channel_index, offset, buffer)? {
                down_channels.push(DownChannel(chan));
            } else {
                tracing::warn!("Buffer for down channel {channel_index} not initialized");
            }
            offset += buffer_size;
        }

        Ok(Rtt {
            ptr,
            up_channels,
            down_channels,
        })
    }

    /// Attempts to detect an RTT control block in the specified RAM region(s) and returns an
    /// instance if a valid control block was found.
    pub fn attach_region(core: &mut Core, region: &ScanRegion) -> Result<Rtt, Error> {
        let ptr = Self::find_contol_block(core, region)?;
        Self::attach_at(core, ptr)
    }

    /// Attempts to detect an RTT control block anywhere in the target RAM and returns an instance
    /// if a valid control block was found.
    pub fn attach(core: &mut Core) -> Result<Rtt, Error> {
        Self::attach_region(core, &ScanRegion::default())
    }

    /// Attempts to detect an RTT control block in the specified RAM region(s) and returns an
    /// address if a valid control block location was found.
    pub fn find_contol_block(core: &mut Core, region: &ScanRegion) -> Result<u64, Error> {
        let ranges = match region.clone() {
            ScanRegion::Exact(addr) => {
                tracing::debug!("Scanning at exact address: {:#010x}", addr);

                return Ok(addr);
            }
            ScanRegion::Ram => {
                tracing::debug!("Scanning whole RAM");

                core.memory_regions()
                    .filter_map(MemoryRegion::as_ram_region)
                    .map(|r| r.range.clone())
                    .collect()
            }
            ScanRegion::Ranges(regions) if regions.is_empty() => {
                // We have no regions to scan so we cannot initialize RTT.
                tracing::debug!(
                    "ELF file has no RTT block symbol, and this target does not support automatic scanning"
                );
                return Err(Error::NoControlBlockLocation);
            }
            ScanRegion::Ranges(regions) => {
                tracing::debug!("Scanning regions: {:#010x?}", region);
                regions
            }
        };

        let mut instances = ranges
            .into_iter()
            .filter_map(|range| {
                let range_len = range.end.checked_sub(range.start)?;
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

                Some(target_ptr)
            })
            .collect::<Vec<_>>();

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

    /// Returns a reference to the detected up channels.
    pub fn up_channels(&mut self) -> &mut [UpChannel] {
        &mut self.up_channels
    }

    /// Returns a reference to the detected down channels.
    pub fn down_channels(&mut self) -> &mut [DownChannel] {
        &mut self.down_channels
    }

    /// Returns a particular up channel.
    pub fn up_channel(&mut self, channel: usize) -> Option<&mut UpChannel> {
        self.up_channels.get_mut(channel)
    }

    /// Returns a particular down channel.
    pub fn down_channel(&mut self, channel: usize) -> Option<&mut DownChannel> {
        self.down_channels.get_mut(channel)
    }

    /// Returns the size of the RTT control block.
    pub fn control_block_size(core: &Core) -> usize {
        let is_64_bit = core.is_64_bit();
        RttControlBlockHeader::minimal_header_size(is_64_bit)
    }
}

/// Used to specify which memory regions to scan for the RTT control block.
#[derive(Clone, Debug, Default)]
pub enum ScanRegion {
    /// Scans all RAM regions known to probe-rs. This is the default and should always work, however
    /// if your device has a lot of RAM, scanning all of it is slow.
    #[default]
    Ram,

    /// Limit scanning to the memory addresses covered by all of the given ranges. It is up to the
    /// user to ensure that reading from this range will not read from undefined memory.
    Ranges(Vec<Range<u64>>),

    /// Tries to find the control block starting at this exact address. It is up to the user to
    /// ensure that reading the necessary bytes after the pointer will no read from undefined
    /// memory.
    Exact(u64),
}

impl ScanRegion {
    /// Creates a new `ScanRegion` that scans the given memory range.
    ///
    /// The memory range should be in a single memory block of the target.
    pub fn range(range: Range<u64>) -> Self {
        Self::Ranges(vec![range])
    }
}

/// Error type for RTT operations.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum Error {
    /// There is no control block location given. This usually means RTT is not present in the
    /// firmware.
    NoControlBlockLocation,

    /// RTT control block not found in target memory.
    /// - Make sure RTT is initialized on the target, AND that there are NO target breakpoints before RTT initialization.
    /// - For VSCode and probe-rs-debugger users, using `halt_after_reset:true` in your `launch.json` file will prevent RTT
    ///   initialization from happening on time.
    /// - Depending on the target, sleep modes can interfere with RTT.
    ControlBlockNotFound,

    /// Multiple control blocks found in target memory: {display_list(_0)}.
    MultipleControlBlocksFound(Vec<u64>),

    /// The control block has been corrupted: {0}
    ControlBlockCorrupted(String),

    /// Attempted an RTT operation against a Core number that is different from the Core number against which RTT was initialized. Expected {0}, found {1}
    IncorrectCoreSpecified(usize, usize),

    /// Error communicating with the probe.
    Probe(#[from] crate::Error),

    /// Unexpected error while reading {0} from target memory. Please report this as a bug.
    MemoryRead(String),

    /// Some uncategorized error occurred.
    Other(#[from] anyhow::Error),

    /// The read pointer changed unexpectedly.
    ReadPointerChanged,

    /// Channel {0} does not exist.
    MissingChannel(usize),
}

fn display_list(list: &[u64]) -> String {
    list.iter()
        .map(|ptr| format!("{ptr:#010x}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn try_attach_to_rtt_inner(
    mut try_attach_once: impl FnMut() -> Result<Rtt, Error>,
    timeout: Duration,
) -> Result<Rtt, Error> {
    let t = Instant::now();
    let mut attempt = 1;
    loop {
        tracing::debug!("Initializing RTT (attempt {attempt})...");

        match try_attach_once() {
            err @ Err(Error::NoControlBlockLocation) => return err,
            Err(_) if t.elapsed() < timeout => {
                attempt += 1;
                tracing::debug!("Failed to initialize RTT. Retrying until timeout.");
                thread::sleep(Duration::from_millis(50));
            }
            other => return other,
        }
    }
}

/// Try to attach to RTT, with the given timeout.
pub fn try_attach_to_rtt(
    core: &mut Core<'_>,
    timeout: Duration,
    rtt_region: &ScanRegion,
) -> Result<Rtt, Error> {
    try_attach_to_rtt_inner(|| Rtt::attach_region(core, rtt_region), timeout)
}

/// Try to attach to RTT, with the given timeout.
pub fn try_attach_to_rtt_shared(
    session: &parking_lot::FairMutex<Session>,
    core_id: usize,
    timeout: Duration,
    rtt_region: &ScanRegion,
) -> Result<Rtt, Error> {
    try_attach_to_rtt_inner(
        || {
            let mut session_handle = session.lock();
            let mut core = session_handle.core(core_id)?;
            Rtt::attach_region(&mut core, rtt_region)
        },
        timeout,
    )
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_how_control_block_list_looks() {
        let error = Error::MultipleControlBlocksFound(vec![0x2000, 0x3000]);
        assert_eq!(
            error.to_string(),
            "Multiple control blocks found in target memory: 0x00002000, 0x00003000."
        );
    }
}
