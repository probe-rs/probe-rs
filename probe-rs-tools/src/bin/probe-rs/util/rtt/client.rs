use crate::util::rtt::{
    ChannelMode, RttActiveDownChannel, RttActiveUpChannel, RttConfig, RttConnection,
};
use probe_rs::{
    Core, MemoryInterface, Target,
    flashing::FlashLoader,
    rtt::{Error, Rtt, ScanRegion},
};

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
            core_id,
        }
    }

    pub fn disallow_clearing_rtt_header(&mut self) {
        self.disallow_clearing_rtt_header = true;
    }

    pub fn configure_from_loader(&mut self, loader: &FlashLoader) {
        // When using RTT with a program in flash, the RTT header will be moved to RAM on
        // startup, so clearing it before startup is ok. However, if we're downloading to the
        // header's final address in RAM, then it's not relocated on startup and we should not
        // clear it. This impacts static RTT headers, like used in defmt_rtt.

        if let ScanRegion::Exact(address) = self.scan_region {
            if loader.has_data_for_address(address) {
                tracing::debug!(
                    "RTT control block is initialized by flash loader. Disabling clearing."
                );
                self.disallow_clearing_rtt_header()
            }
        }
    }

    pub fn is_attached(&self) -> bool {
        self.target.is_some()
    }

    async fn try_attach_impl(&mut self, core: &mut Core<'_>) -> Result<bool, Error> {
        if self.is_attached() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        let location = if let Some(location) = self.last_control_block_address {
            location
        } else {
            let location = match Rtt::find_contol_block(core, &self.scan_region).await {
                Ok(location) => location,
                Err(Error::ControlBlockNotFound) => {
                    tracing::debug!("Failed to attach - control block not found");
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

        let rtt = match Rtt::attach_at(core, location).await {
            Ok(rtt) => rtt,
            Err(Error::ControlBlockNotFound) => {
                self.last_control_block_address = None;
                tracing::debug!("Failed to attach - control block not found");
                return Ok(false);
            }
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("Failed to attach - control block corrupted: {}", error);
                return Ok(false);
            }
            Err(error) => return Err(error),
        };

        match RttConnection::new(rtt).await {
            Ok(rtt) => self.target = Some(rtt),
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("Failed to attach - control block corrupted: {}", error);
            }
            Err(error) => return Err(error),
        };

        Ok(self.target.is_some())
    }

    pub async fn try_attach(&mut self, core: &mut Core<'_>) -> Result<bool, Error> {
        self.try_attach_impl(core).await?;

        if self.need_configure {
            self.configure(core).await?;
            self.need_configure = false;
        }

        Ok(self.is_attached())
    }

    pub async fn poll_channel(
        &mut self,
        core: &mut Core<'_>,
        channel: u32,
    ) -> Result<&[u8], Error> {
        self.try_attach(core).await?;

        if let Some(ref mut target) = self.target {
            match target.poll_channel(core, channel).await {
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

    pub(crate) async fn write_down_channel(
        &mut self,
        core: &mut Core<'_>,
        channel: u32,
        input: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        self.try_attach(core).await?;

        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };

        target.write_down_channel(core, channel, input).await
    }

    pub async fn clean_up(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        self.need_configure = true;

        if let Some(target) = self.target.as_mut() {
            target.clean_up(core).await?;
        }

        Ok(())
    }

    /// This function prevents probe-rs from attaching to an RTT control block that is not
    /// supposed to be valid. This is useful when probe-rs has reset the MCU before attaching,
    /// or during/after flashing, when the MCU has not yet been started.
    pub(crate) async fn clear_control_block(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        if self.disallow_clearing_rtt_header {
            tracing::debug!("Not clearing RTT control block");
            return Ok(());
        }

        self.try_attach(core).await?;

        tracing::debug!("Clearing RTT control block");
        if let Some(mut target) = self.target.take() {
            target.clear_control_block(core).await?;
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
                            core.write_8(location, &[0; SIZE_64B]).await?;
                        } else {
                            const SIZE_32B: usize = 16 + 2 * 4;
                            core.write_8(location, &[0; SIZE_32B]).await?;
                        }
                    }
                } else {
                    // If we have to scan for the location or we somehow found the magic string
                    // somewhere else, we can only clear the magic string.
                    let mut magic = [0; Rtt::RTT_ID.len()];
                    core.read_8(location, &mut magic).await?;
                    if magic == Rtt::RTT_ID {
                        core.write_8(location, &[0; 16]).await?;
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

    pub(crate) async fn configure(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
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
                channel.change_mode(core, mode).await?;
            }
        }

        Ok(())
    }
}
