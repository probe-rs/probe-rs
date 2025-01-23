use crate::util::rtt::{RttActiveDownChannel, RttActiveUpChannel, RttConfig, RttConnection};
use probe_rs::{
    rtt::{Error, Rtt, ScanRegion},
    Core,
};

pub struct RttClient {
    pub scan_region: ScanRegion,
    pub config: RttConfig,

    /// The internal RTT handle, if we have successfully attached to the target.
    target: Option<RttConnection>,

    /// If false, don't try to attach to the target.
    try_attaching: bool,

    /// Whether we have polled data since the last time the control block was corrupted. Used to
    /// prevent spamming the log with messages about corrupted control blocks.
    polled_data: bool,

    /// The core used to poll the target.
    core_id: usize,
}

impl RttClient {
    pub fn new(config: RttConfig, scan_region: ScanRegion) -> Self {
        Self {
            scan_region,
            config,
            target: None,
            try_attaching: true,
            polled_data: false,
            core_id: 0,
        }
    }

    pub fn try_attach(&mut self, core: &mut Core) -> Result<bool, Error> {
        if self.target.is_some() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        match Rtt::attach_region(core, &self.scan_region)
            .and_then(|rtt| RttConnection::new(core, rtt, &self.config).map(Some))
        {
            Ok(rtt) => self.target = rtt,
            Err(Error::ControlBlockNotFound) => {}
            Err(Error::ControlBlockCorrupted(error)) => {
                tracing::debug!("RTT control block corrupted ({error})");
            }
            Err(Error::NoControlBlockLocation) => self.try_attaching = false,
            Err(error) => return Err(error),
        };

        Ok(self.target.is_some())
    }

    pub fn poll_channel(&mut self, core: &mut Core, channel: usize) -> Result<&[u8], Error> {
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

    pub(crate) fn write_down_channel(
        &mut self,
        core: &mut Core,
        channel: usize,
        input: impl AsRef<[u8]>,
    ) -> Result<(), Error> {
        self.try_attach(core)?;

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

    pub(crate) fn clear_control_block(&mut self, core: &mut Core) -> Result<(), Error> {
        self.try_attach(core)?;

        let Some(target) = self.target.as_mut() else {
            // If we can't attach, we don't have a valid
            // control block and don't have to do anything.
            return Ok(());
        };

        target.clear_control_block(core)?;

        self.target = None;

        Ok(())
    }

    pub(crate) fn up_channels(&self) -> &[RttActiveUpChannel] {
        self.target
            .as_ref()
            .map_or(&[], |t| t.active_up_channels.as_slice())
    }

    pub(crate) fn down_channels(&self) -> &[RttActiveDownChannel] {
        self.target
            .as_ref()
            .map_or(&[], |t| t.active_down_channels.as_slice())
    }

    pub(crate) fn core_id(&self) -> usize {
        self.core_id
    }
}
