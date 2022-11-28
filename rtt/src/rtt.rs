use crate::channel::*;
use crate::{Channels, Error};
use probe_rs::{config::MemoryRegion, Core, MemoryInterface};
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
/// the target program has initalized RTT.
///     * This most commonly occurs when the target halts processing before intializing RTT. For example, this could happen ...
///         * During debugging, if the user sets a breakpoint in the code before the RTT initalization.
///         * After flashing, if the user has configured `probe-rs` to `reset_after_flashing` AND `halt_after_reset`. On most targets, this
/// will result in the target halting with reason `Exception` and will delay the subsequent RTT intialization.
///         * If RTT initialization on the target is delayed because of time consuming processing or excessive interrupt handling. This can
/// usually be prevented by moving the RTT intialization code to the very beginning of the target program logic.
///     * The result of such a timing issue is that `probe-rs` will fail to intialize RTT with an [`probe-rs-rtt::Error::ControlBlockNotFound`]
///
/// 3. **Scenario: Incorrect Channel names and incorrect Channel buffer sizes** This scenario usually occurs when two conditions co-incide. Firstly, the same timing mismatch as described in point #2 above, and secondly, the target memory has NOT been cleared since a previous version of the binary program has been flashed to the target.
///     * What happens here is that the RTT Control Block is validated by reading a previously initialized RTT ID from the target memory. The next step in the logic is then to read the Channel configuration from the RTT Control block which is usually contains unreliable data
/// at this point. The symptomps will appear as:
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
                "Nonsensical array sizes at {:08x}: max_up_channels={} max_down_channels={}",
                ptr, max_up_channels, max_down_channels
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
        let ranges: Vec<Range<u32>> = match region {
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
                            start: r.range.start as u32,
                            end: r.range.end as u32,
                        }),
                        _ => None,
                    })
                    .collect()
            }
            ScanRegion::Range(region) => {
                tracing::debug!("Scanning region: {:?}", region);

                vec![region.clone()]
            }
        };

        let mut mem: Vec<u8> = Vec::new();
        let mut instances: Vec<Rtt> = Vec::new();

        for range in ranges.iter() {
            if range.len() < Self::MIN_SIZE {
                continue;
            }

            mem.resize(range.len(), 0);
            {
                core.read(range.start.into(), mem.as_mut())?;
            }

            for offset in 0..(mem.len() - Self::MIN_SIZE) {
                if let Ok(Some(rtt)) = Rtt::from(
                    core,
                    memory_map,
                    range.start + offset as u32,
                    Some(&mem[offset..]),
                ) {
                    instances.push(rtt);

                    if instances.len() >= 5 {
                        break;
                    }
                }
            }
        }

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
#[derive(Clone, Debug)]
pub enum ScanRegion {
    /// Scans all RAM regions known to probe-rs. This is the default and should always work, however
    /// if your device has a lot of RAM, scanning all of it is slow.
    Ram,

    /// Limit scanning to these memory addresses in target memory. It is up to the user to ensure
    /// that reading from this range will not read from undefined memory.
    Range(Range<u32>),

    /// Tries to find the control block starting at this exact address. It is up to the user to
    /// ensure that reading the necessary bytes after the pointer will no read from undefined
    /// memory.
    Exact(u32),
}

impl Default for ScanRegion {
    fn default() -> Self {
        ScanRegion::Ram
    }
}
