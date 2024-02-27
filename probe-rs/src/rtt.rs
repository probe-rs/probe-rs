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
//! use std::sync::{Arc, Mutex};
//! use probe_rs::probe::{list::Lister, Probe};
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

mod syscall;
pub use syscall::*;

pub mod channels;
pub use channels::Channels;

use crate::{config::MemoryRegion, Core, MemoryInterface};
use scroll::{Pread, LE};
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ops::Range;

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
    ptr: u32,
    up_channels: Channels<UpChannel>,
    down_channels: Channels<DownChannel>,
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

    // Minimum size of the ControlBlock struct in target memory in bytes with empty arrays
    const MIN_SIZE: usize = Self::O_CHANNEL_ARRAYS;

    // Offsets of fields in target memory in bytes
    const O_ID: usize = 0;
    const O_MAX_UP_CHANNELS: usize = 16;
    const O_MAX_DOWN_CHANNELS: usize = 20;
    const O_CHANNEL_ARRAYS: usize = 24;

    fn from(
        core: &mut Core,
        memory_map: &[MemoryRegion],
        // Pointer from which to scan
        ptr: u32,
        // Memory contents read in advance, starting from ptr
        mem_in: Option<&[u8]>,
    ) -> Result<Option<Rtt>, Error> {
        let mut mem = match mem_in {
            Some(mem) => Cow::Borrowed(mem),
            None => {
                // If memory wasn't passed in, read the minimum header size
                let mut mem = vec![0u8; Self::MIN_SIZE];
                core.read(ptr.into(), &mut mem)?;
                Cow::Owned(mem)
            }
        };

        // Validate that the control block starts with the ID bytes
        let rtt_id = &mem[Self::O_ID..(Self::O_ID + Self::RTT_ID.len())];
        if rtt_id != Self::RTT_ID {
            tracing::trace!(
                "Expected control block to start with RTT ID: {:?}\n. Got instead: {:?}",
                String::from_utf8_lossy(&Self::RTT_ID),
                String::from_utf8_lossy(rtt_id)
            );
            return Err(Error::ControlBlockNotFound);
        }

        let max_up_channels = mem.pread_with::<u32>(Self::O_MAX_UP_CHANNELS, LE).unwrap() as usize;
        let max_down_channels = mem
            .pread_with::<u32>(Self::O_MAX_DOWN_CHANNELS, LE)
            .unwrap() as usize;

        // *Very* conservative sanity check, most people
        if max_up_channels > 255 || max_down_channels > 255 {
            return Err(Error::ControlBlockCorrupted(format!(
                "Nonsensical array sizes at {ptr:08x}: max_up_channels={max_up_channels} max_down_channels={max_down_channels}"
            )));
        }

        let cb_len = Self::O_CHANNEL_ARRAYS + (max_up_channels + max_down_channels) * Channel::SIZE;

        if let Cow::Owned(mem) = &mut mem {
            // If memory wasn't passed in, read the rest of the control block
            mem.resize(cb_len, 0);
            core.read(
                (ptr + Self::MIN_SIZE as u32).into(),
                &mut mem[Self::MIN_SIZE..cb_len],
            )?;
        }

        // Validate that the entire control block fits within the region
        if mem.len() < cb_len {
            tracing::debug!("Control block doesn't fit in scanned memory region.");
            return Ok(None);
        }

        let mut up_channels = BTreeMap::new();
        let mut down_channels = BTreeMap::new();

        for i in 0..max_up_channels {
            let offset = Self::O_CHANNEL_ARRAYS + i * Channel::SIZE;

            if let Some(chan) =
                Channel::from(core, i, memory_map, ptr + offset as u32, &mem[offset..])?
            {
                up_channels.insert(i, UpChannel(chan));
            } else {
                tracing::warn!("Buffer for up channel {} not initialized", i);
            }
        }

        for i in 0..max_down_channels {
            let offset =
                Self::O_CHANNEL_ARRAYS + (max_up_channels * Channel::SIZE) + i * Channel::SIZE;

            if let Some(chan) =
                Channel::from(core, i, memory_map, ptr + offset as u32, &mem[offset..])?
            {
                down_channels.insert(i, DownChannel(chan));
            } else {
                tracing::warn!("Buffer for down channel {} not initialized", i);
            }
        }

        Ok(Some(Rtt {
            ptr,
            up_channels: Channels(up_channels),
            down_channels: Channels(down_channels),
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
        let ranges: Vec<Range<u64>> = match region {
            ScanRegion::Exact(addr) => {
                tracing::debug!("Scanning at exact address: 0x{:X}", addr);

                return Rtt::from(core, memory_map, *addr, None)?
                    .ok_or(Error::ControlBlockNotFound);
            }
            ScanRegion::Ram => {
                tracing::debug!("Scanning RAM");

                memory_map
                    .iter()
                    .filter_map(|r| match r {
                        MemoryRegion::Ram(r) => Some(Range {
                            start: r.range.start,
                            end: r.range.end,
                        }),
                        _ => None,
                    })
                    .collect()
            }
            ScanRegion::Ranges(regions) => regions.clone(),
            ScanRegion::Range(region) => {
                tracing::debug!("Scanning region: {:?}", region);

                vec![Range {
                    start: region.start as u64,
                    end: region.end as u64,
                }]
            }
        };

        let mut instances = ranges
            .into_iter()
            .filter_map(|range| {
                let range_len = match range.end.checked_sub(range.start) {
                    Some(v) => if v < (Self::MIN_SIZE as u64) {
                        return None;
                    } else {
                        v
                    },
                    None => return None,
                };

                let range_len_usize: usize = match range_len.try_into() {
                    Ok(v) => v,
                    Err(_) => {
                        // FIXME: This is not ideal because it means that we
                        // won't consider a >4GiB region if probe-rs is running
                        // on a 32-bit host, but it would be relatively unusual
                        // to use a 32-bit host to debug a 64-bit target.
                        tracing::warn!("ignoring region of length {} because it is too long to buffer in host memory", range_len);
                        return None;
                    }
                };

                let mut mem = vec![0; range_len_usize];
                {
                    core.read(range.start, mem.as_mut()).ok()?;
                }

                match kmp::kmp_find(&Self::RTT_ID, mem.as_slice()) {
                    Some(offset) => {
                        let target_ptr = range.start + (offset as u64);
                        let target_ptr: u32 = match target_ptr.try_into() {
                            Ok(v) => v,
                            Err(_) => {
                                // FIXME: The RTT API currently supports only
                                // 32-bit addresses, and so it can't accept
                                // an RTT block at an address >4GiB.
                                tracing::warn!("can't use RTT block at {:#010x}; must be at a location reachable by 32-bit addressing", target_ptr);
                                return None;
                            },
                        };

                        Rtt::from(
                            core,
                            memory_map,
                            target_ptr,
                            Some(&mem[offset..]),
                        )
                        .transpose()
                    },
                    None => None,
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        if instances.is_empty() {
            return Err(Error::ControlBlockNotFound);
        }

        if instances.len() > 1 {
            return Err(Error::MultipleControlBlocksFound(
                instances.into_iter().map(|i| i.ptr).collect(),
            ));
        }

        Ok(instances.remove(0))
    }

    /// Returns the memory address of the control block in target memory.
    pub fn ptr(&self) -> u32 {
        self.ptr
    }

    /// Gets the detected up channels.
    pub fn up_channels(&mut self) -> &mut Channels<UpChannel> {
        &mut self.up_channels
    }

    /// Gets the detected down channels.
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
    Range(Range<u32>),

    /// Limit scanning to the memory addresses covered by all of the given ranges. It is up to the
    /// user to ensure that reading from this range will not read from undefined memory.
    Ranges(Vec<Range<u64>>),

    /// Tries to find the control block starting at this exact address. It is up to the user to
    /// ensure that reading the necessary bytes after the pointer will no read from undefined
    /// memory.
    Exact(u32),
}

/// Error type for RTT operations.
#[derive(thiserror::Error, Debug)]
pub enum Error {
    /// RTT control block not found in target memory. Make sure RTT is initialized on the target.
    #[error(
        "RTT control block not found in target memory.\n\
        - Make sure RTT is initialized on the target, AND that there are NO target breakpoints before RTT initalization.\n\
        - For VSCode and probe-rs-debugger users, using `halt_after_reset:true` in your `launch.json` file will prevent RTT \n\
        \tinitialization from happening on time.\n\
        - Depending on the target, sleep modes can interfere with RTT."
    )]
    ControlBlockNotFound,

    /// Multiple control blocks found in target memory. The data contains the control block addresses (up to 5).
    #[error("Multiple control blocks found in target memory.")]
    MultipleControlBlocksFound(Vec<u32>),

    /// The control block has been corrupted. The data contains a detailed error.
    #[error("Control block corrupted: {0}")]
    ControlBlockCorrupted(String),

    /// Attempted an RTT read/write operation against a Core number that is different from the Core number against which RTT was initialized
    #[error("Incorrect Core number specified for this operation. Expected {0}, and found {1}")]
    IncorrectCoreSpecified(usize, usize),

    /// Wraps errors propagated up from probe-rs.
    #[error("Error communicating with probe: {0}")]
    Probe(#[from] crate::Error),

    /// Wraps errors propagated up from reading memory on the target.
    #[error("Unexpected error while reading {0} from target memory. Please report this as a bug.")]
    MemoryRead(String),
}
