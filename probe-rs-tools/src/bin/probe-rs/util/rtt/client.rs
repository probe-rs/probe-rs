use std::sync::Arc;

use crate::util::rtt::{
    ChannelDataCallbacks, DefmtState, RttActiveDownChannel, RttActiveTarget, RttActiveUpChannel,
    RttConfig, RttSymbolError,
};
use probe_rs::{
    rtt::{Error, Rtt, ScanRegion},
    Core, MemoryInterface, Target,
};
use time::UtcOffset;

pub struct RttClient {
    pub defmt_data: Option<Arc<DefmtState>>,
    pub scan_region: ScanRegion,
    pub timezone_offset: UtcOffset,
    rtt_config: RttConfig,

    /// The internal RTT handle, if we have successfully attached to the target.
    target: Option<RttActiveTarget>,

    /// If false, don't try to attach to the target.
    try_attaching: bool,

    /// Whether we have polled data since the last time the control block was corrupted. Used to
    /// prevent spamming the log with messages about corrupted control blocks.
    polled_data: bool,

    /// The core used to poll the target.
    core_id: usize,
}

impl RttClient {
    pub fn new(
        elf: Option<&[u8]>,
        target: &Target,
        rtt_config: RttConfig,
        scan_region: ScanRegion,
    ) -> Result<Self, Error> {
        let mut this = Self {
            defmt_data: None,
            scan_region,
            rtt_config,
            target: None,
            try_attaching: true,
            timezone_offset: UtcOffset::UTC,
            polled_data: false,
            core_id: 0,
        };

        if let Some(elf) = elf {
            let mut init_defmt = false;
            match RttActiveTarget::get_rtt_symbol_from_bytes(elf) {
                Ok(address) => {
                    this.scan_region = ScanRegion::Exact(address);
                    this.core_id = target.core_index_by_address(address).unwrap_or(0);

                    init_defmt = true;
                }
                Err(RttSymbolError::Goblin(_)) => {
                    // Not an ELF
                }
                Err(RttSymbolError::RttSymbolNotFound) => {
                    // We can still try to use defmt, we might find the control block.
                    init_defmt = true;
                }
            }

            if init_defmt {
                this.defmt_data = DefmtState::try_from_bytes(elf)?.map(Arc::new);
            }
        }

        Ok(this)
    }

    pub fn try_attach(&mut self, core: &mut Core) -> Result<bool, Error> {
        self.try_attach_with_address(core).map_err(|(e, _)| e)
    }

    fn try_attach_with_address(&mut self, core: &mut Core) -> Result<bool, (Error, Option<u64>)> {
        if self.target.is_some() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        match Rtt::attach_region_with_address(core, &self.scan_region).and_then(|rtt| {
            let rtt_ptr = rtt.ptr();
            RttActiveTarget::new(
                core,
                rtt,
                self.defmt_data.clone(),
                &self.rtt_config,
                self.timezone_offset,
            )
            .map_err(|e| (e, Some(rtt_ptr)))
            .map(Some)
        }) {
            Ok(rtt) => self.target = rtt,
            Err((Error::ControlBlockNotFound, _)) => {}
            Err((Error::NoControlBlockLocation, _)) => self.try_attaching = false,
            Err(error) => return Err(error),
        };

        Ok(self.target.is_some())
    }

    pub fn poll(
        &mut self,
        core: &mut Core,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<(), Error> {
        self.try_attach(core)?;

        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };

        let result = target.poll_rtt_fallible(core, collector);
        self.handle_poll_result(result)
    }

    pub fn poll_channel(
        &mut self,
        core: &mut Core,
        channel: usize,
        collector: &mut impl ChannelDataCallbacks,
    ) -> Result<(), Error> {
        self.try_attach(core)?;

        let Some(target) = self.target.as_mut() else {
            return Ok(());
        };

        let result = target.poll_channel_fallible(core, channel, collector);
        self.handle_poll_result(result)
    }

    fn handle_poll_result(&mut self, result: Result<(), Error>) -> Result<(), Error> {
        match result {
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
            other => return other,
        }
        Ok(())
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
        let backup_address = match self.try_attach_with_address(core) {
            Ok(_) => 0,
            Err((e, None)) => return Err(e),
            Err((_, Some(addr))) => addr,
        };

        let Some(target) = self.target.as_mut() else {
            // If we can't attach, but we do have the address of the control block
            let zeros = vec![0; Rtt::control_block_size(core)];
            core.write(backup_address, &zeros)?;

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
