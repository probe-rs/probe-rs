use crate::util::rtt::{RttActiveDownChannel, RttActiveUpChannel, RttConfig, RttConnection};
use probe_rs::{
    Core, MemoryInterface, Target,
    flashing::FlashLoader,
    rtt::{Error, Rtt, ScanRegion},
};
use probe_rs_rpc::rtt_config::ChannelMode;
use std::time::{Duration, Instant};

/// How long the control block may be missing before the user is told about it.
const MISSING_CONTROL_BLOCK_WARNING_DELAY: Duration = Duration::from_secs(5);

pub struct RttClient {
    pub scan_region: ScanRegion,
    channel_modes: Vec<Option<ChannelMode>>,
    need_configure: bool,

    /// The internal RTT handle, if we have successfully attached to the target.
    target: Option<RttConnection>,
    last_control_block_address: Option<u64>,

    /// If the control block is initialized by the flasher, this flag is used to prevent
    /// clearing the control block when the target is reset.
    disallow_clearing_rtt_header: bool,

    /// If false, don't try to attach to the target.
    try_attaching: bool,

    /// Whether we have polled data since the last time the control block was corrupted. Used to
    /// prevent spamming the log with messages about corrupted control blocks.
    polled_data: bool,

    /// When we first failed to find the control block since we were last attached.
    control_block_missing_since: Option<Instant>,

    /// Whether the user has been told that the control block is missing. Used to only warn
    /// once per attach.
    warned_missing_control_block: bool,

    /// The core used to poll the target.
    core_id: usize,
}

impl RttClient {
    pub fn new(config: RttConfig, scan_region: ScanRegion, target: &Target) -> Self {
        let core_id = if let ScanRegion::Exact(address) = scan_region {
            target.core_index_by_address(address).unwrap_or(0)
        } else {
            0
        };

        Self {
            scan_region,
            channel_modes: config.channels.iter().map(|c| c.mode).collect(),
            need_configure: true,

            target: None,
            last_control_block_address: None,
            disallow_clearing_rtt_header: false,
            try_attaching: true,
            polled_data: false,
            control_block_missing_since: None,
            warned_missing_control_block: false,
            core_id,
        }
    }

    /// Learn from `loader` whether the download initializes the RTT control
    /// block, and allow or disallow clearing the block accordingly.
    ///
    /// When using RTT with a program in flash, the RTT header will be moved to RAM on
    /// startup, so clearing it before startup is ok. However, if we're downloading to the
    /// header's final address in RAM, then it's not relocated on startup and we should not
    /// clear it. This impacts static RTT headers, like used in defmt_rtt.
    pub fn configure_from_loader(&mut self, loader: &FlashLoader) {
        // An image replaces the one this client learned from, thus this
        // assigns rather than only disallows.
        self.disallow_clearing_rtt_header = match self.scan_region {
            ScanRegion::Exact(address) => loader.has_data_for_address(address),
            _ => false,
        };

        if self.disallow_clearing_rtt_header {
            tracing::debug!(
                "RTT control block is initialized by flash loader. Disabling clearing."
            );
        }
    }

    pub fn is_attached(&self) -> bool {
        self.target.is_some()
    }

    fn try_attach_impl(&mut self, core: &mut Core) -> Result<bool, Error> {
        if self.is_attached() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        let location = if let Some(location) = self.last_control_block_address {
            location
        } else {
            let location = match Rtt::find_control_block(core, &self.scan_region) {
                Ok(location) => location,
                Err(Error::ControlBlockNotFound) => {
                    tracing::debug!("Failed to attach - control block not found");
                    self.control_block_missing_since
                        .get_or_insert_with(Instant::now);
                    return Ok(false);
                }
                Err(Error::NoControlBlockLocation) => {
                    tracing::debug!("Failed to attach - control block location not specified");
                    self.try_attaching = false;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            };

            self.last_control_block_address = Some(location);
            location
        };

        let rtt = match Rtt::attach_at(core, location) {
            Ok(rtt) => rtt,
            Err(Error::ControlBlockNotFound) => {
                self.last_control_block_address = None;
                tracing::debug!("Failed to attach - control block not found");
                self.control_block_missing_since
                    .get_or_insert_with(Instant::now);
                return Ok(false);
            }
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("Failed to attach - control block corrupted: {}", error);
                return Ok(false);
            }
            Err(error) => return Err(error),
        };

        match RttConnection::new(rtt) {
            Ok(rtt) => {
                self.target = Some(rtt);
                self.control_block_missing_since = None;
                self.warned_missing_control_block = false;
            }
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("Failed to attach - control block corrupted: {}", error);
            }
            Err(error) => return Err(error),
        };

        self.need_configure = true;
        Ok(self.target.is_some())
    }

    pub fn try_attach(&mut self, core: &mut Core) -> Result<bool, Error> {
        let attached = self.try_attach_impl(core)?;

        if attached && self.need_configure {
            self.configure(core)?;
            self.need_configure = false;
        }

        Ok(self.is_attached())
    }

    /// Returns true once the control block has been missing for `delay`, and only the first
    /// time that is the case since we were last attached.
    fn control_block_went_missing(&mut self, delay: Duration) -> bool {
        let missing = self
            .control_block_missing_since
            .is_some_and(|since| since.elapsed() >= delay);

        if !missing || self.warned_missing_control_block {
            return false;
        }

        self.warned_missing_control_block = true;
        true
    }

    /// Tells the user, once, that the control block has not shown up. Attaching fails silently
    /// while the firmware has yet to initialize RTT, which looks the same as firmware that
    /// never does, or an ELF file that does not match the firmware on the target.
    pub(crate) fn warn_if_control_block_is_missing(&mut self) {
        if !self.control_block_went_missing(MISSING_CONTROL_BLOCK_WARNING_DELAY) {
            return;
        }

        let delay = MISSING_CONTROL_BLOCK_WARNING_DELAY.as_secs();
        match self.scan_region {
            ScanRegion::Exact(address) => tracing::warn!(
                "No RTT control block found at {address:#010x} after {delay} seconds. \
                 The ELF file may not match the firmware on the target, or the firmware \
                 has not initialized RTT. Still trying to attach."
            ),
            _ => tracing::warn!(
                "No RTT control block found after {delay} seconds. The firmware may not \
                 have initialized RTT. Still trying to attach."
            ),
        }
    }

    pub fn poll_channel(&mut self, core: &mut Core, channel: u32) -> Result<&[u8], Error> {
        self.try_attach(core)?;

        if let Some(ref mut target) = self.target {
            match target.poll_channel(core, channel) {
                Ok(()) => self.polled_data = true,

                Err(Error::ControlBlockCorrupted(error)) => {
                    if self.polled_data {
                        tracing::warn!("RTT control block corrupted ({error}), re-attaching");
                    }
                    self.target = None;
                    self.polled_data = false;
                }
                Err(Error::ReadPointerChanged) => {
                    if self.polled_data {
                        tracing::warn!("RTT read pointer changed, re-attaching");
                    }
                    self.target = None;
                    self.polled_data = false;
                }

                Err(other) => return Err(other),
            }
        }

        if let Some(ref target) = self.target {
            return target.channel_data(channel);
        }

        Ok(&[])
    }

    /// Writes as much of `input` as the target accepts, returning that count.
    /// Returns 0 if RTT is not attached yet, in which case nothing was sent.
    pub(crate) fn write_down_channel(
        &mut self,
        core: &mut Core,
        channel: u32,
        input: impl AsRef<[u8]>,
    ) -> Result<usize, Error> {
        self.try_attach(core)?;

        let Some(target) = self.target.as_mut() else {
            return Ok(0);
        };

        target.write_down_channel(core, channel, input)
    }

    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        self.need_configure = true;

        if let Some(target) = self.target.as_mut() {
            target.clean_up(core)?;
        }

        Ok(())
    }

    /// This function prevents probe-rs from attaching to an RTT control block that is not
    /// supposed to be valid. This is useful when probe-rs has reset the MCU before attaching,
    /// or during/after flashing, when the MCU has not yet been started.
    pub(crate) fn clear_control_block(&mut self, core: &mut Core) -> Result<(), Error> {
        if self.disallow_clearing_rtt_header {
            tracing::debug!("Not clearing RTT control block");
            return Ok(());
        }

        self.try_attach_impl(core)?;

        tracing::debug!("Clearing RTT control block");
        if let Some(mut target) = self.target.take() {
            target.clear_control_block(core)?;
        } else {
            // While the entire block isn't valid in itself, some parts of it may be.
            // Depending on the firmware, the control block may be initialized in such
            // an order where probe-rs can attach to it before it is fully valid.
            if let Some(location) = self.last_control_block_address.take() {
                if let ScanRegion::Exact(scan_location) = self.scan_region {
                    // If we know the exact location where a control block should be, we can clear
                    // the whole block.
                    if location == scan_location {
                        if core.is_64_bit() {
                            const SIZE_64B: usize = 16 + 2 * 8;
                            core.write_8(location, &[0; SIZE_64B])?;
                        } else {
                            const SIZE_32B: usize = 16 + 2 * 4;
                            core.write_8(location, &[0; SIZE_32B])?;
                        }
                    }
                } else {
                    // If we have to scan for the location or we somehow found the magic string
                    // somewhere else, we can only clear the magic string.
                    let mut magic = [0; Rtt::RTT_ID.len()];
                    core.read_8(location, &mut magic)?;
                    if magic == Rtt::RTT_ID {
                        core.write_8(location, &[0; 16])?;
                    }
                }
            }

            // There's nothing we can do if we don't know where the control block is.
        }

        Ok(())
    }

    pub(crate) fn up_channels(&self) -> &[RttActiveUpChannel] {
        self.target
            .as_ref()
            .map(|t| t.active_up_channels.as_slice())
            .unwrap_or_default()
    }

    pub(crate) fn down_channels(&self) -> &[RttActiveDownChannel] {
        self.target
            .as_ref()
            .map(|t| t.active_down_channels.as_slice())
            .unwrap_or_default()
    }

    pub(crate) fn core_id(&self) -> usize {
        self.core_id
    }

    pub(crate) fn configure(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };

        for channel in target.active_up_channels.as_mut_slice() {
            let channel_mode = self
                .channel_modes
                .get(channel.up_channel.number())
                .copied()
                .unwrap_or_else(|| {
                    if channel.channel_name() == "defmt" {
                        // defmt channel is always blocking
                        Some(ChannelMode::BlockIfFull)
                    } else {
                        None
                    }
                });

            if let Some(mode) = channel_mode {
                channel.change_mode(core, mode)?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use probe_rs::config::Registry;

    const CONTROL_BLOCK: u64 = 0x2000_0000;
    const ELSEWHERE: u64 = 0x2000_1000;

    fn target() -> Target {
        Registry::from_builtin_families()
            .get_target_by_name("nRF52833_xxAA")
            .unwrap()
    }

    fn loader_writing(target: &Target, address: u64) -> FlashLoader {
        let mut loader = target.flash_loader();
        loader.add_data(address, &[0; 16]).unwrap();
        loader
    }

    fn client(target: &Target) -> RttClient {
        RttClient::new(
            RttConfig::default(),
            ScanRegion::Exact(CONTROL_BLOCK),
            target,
        )
    }

    #[test]
    fn a_control_block_that_the_download_writes_is_not_cleared() {
        let target = target();
        let mut client = client(&target);

        client.configure_from_loader(&loader_writing(&target, CONTROL_BLOCK));

        assert!(client.disallow_clearing_rtt_header);
    }

    #[test]
    fn a_control_block_that_the_program_initializes_is_cleared() {
        let target = target();
        let mut client = client(&target);

        client.configure_from_loader(&loader_writing(&target, ELSEWHERE));

        assert!(!client.disallow_clearing_rtt_header);
    }

    #[test]
    fn a_later_image_decides_again_whether_the_control_block_is_cleared() {
        let target = target();
        let mut client = client(&target);

        client.configure_from_loader(&loader_writing(&target, CONTROL_BLOCK));
        client.configure_from_loader(&loader_writing(&target, ELSEWHERE));

        assert!(!client.disallow_clearing_rtt_header);
    }

    #[test]
    fn a_scanned_control_block_is_cleared() {
        let target = target();
        let mut client = RttClient::new(RttConfig::default(), ScanRegion::Ram, &target);

        client.configure_from_loader(&loader_writing(&target, CONTROL_BLOCK));

        assert!(!client.disallow_clearing_rtt_header);
    }

    #[test]
    fn a_control_block_that_was_not_looked_for_is_not_missing() {
        let target = target();
        let mut client = client(&target);

        assert!(!client.control_block_went_missing(Duration::ZERO));
    }

    #[test]
    fn a_control_block_is_not_missing_before_the_delay_has_passed() {
        let target = target();
        let mut client = client(&target);
        client.control_block_missing_since = Some(Instant::now());

        assert!(!client.control_block_went_missing(Duration::from_secs(3600)));
    }

    #[test]
    fn a_missing_control_block_is_reported_once() {
        let target = target();
        let mut client = client(&target);
        client.control_block_missing_since = Some(Instant::now());

        assert!(client.control_block_went_missing(Duration::ZERO));
        assert!(!client.control_block_went_missing(Duration::ZERO));
    }
}
