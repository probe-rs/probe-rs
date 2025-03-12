use crate::util::rtt::{
    ChannelMode, RttActiveDownChannel, RttActiveUpChannel, RttConfig, RttConnection,
};
use probe_rs::{
    Core, MemoryInterface,
    rtt::{Error, Rtt, ScanRegion},
};

pub struct InnerRttClient {
    /// The internal RTT handle, if we have successfully attached to the target.
    target: Option<RttConnection>,
    last_control_block_address: Option<u64>,

    /// If false, don't try to attach to the target.
    try_attaching: bool,

    /// Whether we have polled data since the last time the control block was corrupted. Used to
    /// prevent spamming the log with messages about corrupted control blocks.
    polled_data: bool,

    /// The core used to poll the target.
    core_id: usize,
}

impl InnerRttClient {
    pub fn new() -> Self {
        Self {
            target: None,
            last_control_block_address: None,
            try_attaching: true,
            polled_data: false,
            core_id: 0,
        }
    }

    pub fn is_attached(&self) -> bool {
        self.target.is_some()
    }

    pub fn try_attach(&mut self, core: &mut Core, scan_region: &ScanRegion) -> Result<bool, Error> {
        if self.is_attached() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        let location = if let Some(location) = self.last_control_block_address {
            location
        } else {
            let location = match Rtt::find_contol_block(core, scan_region) {
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

        let rtt = match Rtt::attach_at(core, location) {
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

        match RttConnection::new(rtt) {
            Ok(rtt) => self.target = Some(rtt),
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("Failed to attach - control block corrupted: {}", error);
            }
            Err(error) => return Err(error),
        };

        Ok(self.target.is_some())
    }

    pub fn poll_channel(&mut self, core: &mut Core, channel: u32) -> Result<&[u8], Error> {
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

    pub(crate) fn write_down_channel(
        &mut self,
        core: &mut Core,
        channel: u32,
        input: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };

        target.write_down_channel(core, channel, input)
    }

    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        if let Some(target) = self.target.as_mut() {
            target.clean_up(core)?;
        }

        Ok(())
    }

    /// This function prevents probe-rs from attaching to an RTT control block that is not
    /// supposed to be valid. This is useful when probe-rs has reset the MCU before attaching,
    /// or during/after flashing, when the MCU has not yet been started.
    pub(crate) fn clear_control_block(
        &mut self,
        core: &mut Core,
        scan_region: &ScanRegion,
    ) -> Result<(), Error> {
        if let Some(mut target) = self.target.take() {
            target.clear_control_block(core)?;
        } else {
            // While the entire block isn't valid in itself, some parts of it may be.
            // Depending on the firmware, the control block may be initialized in such
            // an order where probe-rs can attach to it before it is fully valid.
            if let Some(location) = self.last_control_block_address.take() {
                if let ScanRegion::Exact(scan_location) = scan_region {
                    // If we know the exact location where a control block should be, we can clear
                    // the whole block.
                    if location == *scan_location {
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
}

pub struct RttClient {
    pub scan_region: ScanRegion,
    config: RttConfig,
    client: InnerRttClient,
    need_configure: bool,
}

impl RttClient {
    pub fn new(config: RttConfig, scan_region: ScanRegion) -> Self {
        Self {
            scan_region,
            config,
            client: InnerRttClient::new(),
            need_configure: true,
        }
    }

    pub fn is_attached(&self) -> bool {
        self.client.is_attached()
    }

    pub fn try_attach(&mut self, core: &mut Core) -> Result<bool, Error> {
        self.client.try_attach(core, &self.scan_region)?;

        if self.need_configure {
            self.configure(core)?;
            self.need_configure = false;
        }

        Ok(self.is_attached())
    }

    pub fn poll_channel(&mut self, core: &mut Core, channel: u32) -> Result<&[u8], Error> {
        self.try_attach(core)?;
        self.client.poll_channel(core, channel)
    }

    pub(crate) fn write_down_channel(
        &mut self,
        core: &mut Core,
        channel: u32,
        input: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        self.try_attach(core)?;
        self.client.write_down_channel(core, channel, input)
    }

    pub fn clean_up(&mut self, core: &mut Core) -> Result<(), Error> {
        self.need_configure = true;
        self.client.clean_up(core)
    }

    /// This function prevents probe-rs from attaching to an RTT control block that is not
    /// supposed to be valid. This is useful when probe-rs has reset the MCU before attaching,
    /// or during/after flashing, when the MCU has not yet been started.
    pub(crate) fn clear_control_block(&mut self, core: &mut Core) -> Result<(), Error> {
        self.try_attach(core)?;
        self.client.clear_control_block(core, &self.scan_region)
    }

    pub(crate) fn up_channels(&self) -> &[RttActiveUpChannel] {
        self.client.up_channels()
    }

    pub(crate) fn down_channels(&self) -> &[RttActiveDownChannel] {
        self.client.down_channels()
    }

    pub(crate) fn core_id(&self) -> usize {
        self.client.core_id()
    }

    pub(crate) fn configure(&mut self, core: &mut Core<'_>) -> Result<(), Error> {
        let Some(target) = self.client.target.as_mut() else {
            return Ok(());
        };

        let up_channels = target.active_up_channels.as_mut_slice();

        for (idx, channel) in up_channels.iter_mut().enumerate() {
            let channel_mode = if let Some(config) = self.config.channel_config(idx as u32) {
                config.mode
            } else if channel.channel_name() == "defmt" {
                // defmt channel is always blocking
                Some(ChannelMode::BlockIfFull)
            } else {
                None
            };

            if let Some(mode) = channel_mode {
                channel.change_mode(core, mode)?;
            }
        }

        Ok(())
    }
}
