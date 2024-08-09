use std::sync::Arc;

use crate::util::rtt::{ChannelDataCallbacks, DefmtState, RttActiveTarget, RttConfig};
use probe_rs::{
    rtt::{Error, Rtt, ScanRegion},
    Core,
};
use time::UtcOffset;

pub struct RttClient {
    pub defmt_data: Option<Arc<DefmtState>>,
    pub scan_region: ScanRegion,
    rtt_config: RttConfig,
    target: Option<RttActiveTarget>,
    try_attaching: bool,
    pub timezone_offset: UtcOffset,
    polled_data: bool,
}

impl RttClient {
    pub fn new(
        elf: Option<&[u8]>,
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
        };

        if let Some(elf) = elf {
            if let Some(address) = RttActiveTarget::get_rtt_symbol_from_bytes(elf) {
                this.scan_region = ScanRegion::Exact(address);
            }
            this.defmt_data = DefmtState::try_from_bytes(elf)?.map(Arc::new);
        }

        Ok(this)
    }

    pub fn try_attach(&mut self, core: &mut Core) -> Result<bool, Error> {
        if self.target.is_some() {
            return Ok(true);
        }

        if !self.try_attaching {
            return Ok(false);
        }

        match Rtt::attach_region(core, &self.scan_region).and_then(|rtt| {
            RttActiveTarget::new(
                core,
                rtt,
                self.defmt_data.clone(),
                &self.rtt_config,
                self.timezone_offset,
            )
            .map(Some)
        }) {
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

    pub(crate) fn into_target(self) -> RttActiveTarget {
        self.target.unwrap()
    }
}
