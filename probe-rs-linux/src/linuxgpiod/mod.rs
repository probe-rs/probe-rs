//! Linux GPIO bit-bang SWD probe driver.

mod error;
mod pins;
mod swd;

use std::fmt;

use probe_rs::Error;
use probe_rs::probe::{
    BitbangSwd, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector, IoSequenceItem,
    ProbeCreationError, ProbeFactory, SwdProbe, SwdSettings, WireProtocol, list::ProbeListItem,
    swd::Pins,
};

use self::error::LinuxGpiodError;
use self::pins::PinMap;
use self::swd::SwdBus;

/// Linux GPIO bit-bang SWD probe.
///
/// Selectors take the form
/// `0:0:<gpiochip>,swclk=<offset>,swdio=<offset>[,srst=<offset>]` where
/// `<gpiochip>` is `gpiochipN`, `/dev/gpiochipN`, or just `N`.
pub struct LinuxGpiod {
    bus: SwdBus,
    speed_khz: u32,
    swd_settings: SwdSettings,
}

impl fmt::Debug for LinuxGpiod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinuxGpiod")
            .field("speed_khz", &self.speed_khz)
            .finish_non_exhaustive()
    }
}

impl LinuxGpiod {
    fn open(pins: &PinMap) -> Result<Self, LinuxGpiodError> {
        let request = pins.request()?;
        Ok(Self {
            bus: SwdBus::new(request, pins.swclk, pins.swdio, pins.srst),
            speed_khz: 0,
            swd_settings: SwdSettings::default(),
        })
    }
}

impl DebugProbe for LinuxGpiod {
    fn get_name(&self) -> &str {
        "Linux GPIO bit-bang SWD"
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        // No-op: bit-bang runs as fast as the kernel allows.
        self.speed_khz = speed_khz;
        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn detach(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.target_reset_assert()?;
        std::thread::sleep(std::time::Duration::from_millis(20));
        self.target_reset_deassert()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        if !self
            .bus
            .drive_srst(false)
            .map_err(|e| DebugProbeError::ProbeSpecific(e.into()))?
        {
            return Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset_assert (SRST not routed)",
            });
        }
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        if !self
            .bus
            .drive_srst(true)
            .map_err(|e| DebugProbeError::ProbeSpecific(e.into()))?
        {
            return Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset_deassert (SRST not routed)",
            });
        }
        Ok(())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Swd => Ok(()),
            WireProtocol::Jtag => Err(DebugProbeError::UnsupportedProtocol(protocol)),
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(WireProtocol::Swd)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_as_swd_probe(self: Box<Self>) -> Result<Box<dyn SwdProbe>, Box<dyn DebugProbe>> {
        Ok(self)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

impl BitbangSwd for LinuxGpiod {
    fn swd_io<S>(&mut self, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
        self.bus
            .transfer(swdio)
            .map_err(|e| DebugProbeError::ProbeSpecific(e.into()))
    }

    fn swj_pins_op(
        &mut self,
        out: Pins,
        select: Pins,
        _wait: std::time::Duration,
    ) -> Result<(), DebugProbeError> {
        const N_RESET: u8 = 1 << 7;
        if select.0 & !N_RESET != 0 {
            return Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins (only nRESET is controllable)",
            });
        }
        if select.nreset() {
            let high = out.nreset();
            if !self
                .bus
                .drive_srst(high)
                .map_err(|e| DebugProbeError::ProbeSpecific(e.into()))?
            {
                return Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "swj_pins nRESET (SRST not routed)",
                });
            }
        }
        Ok(())
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }
}

/// Factory for [`LinuxGpiod`] probes.
#[derive(Debug)]
pub struct LinuxGpiodFactory;

impl fmt::Display for LinuxGpiodFactory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Linux GPIO bit-bang SWD")
    }
}

impl ProbeFactory for LinuxGpiodFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        // We're never a USB device; let other factories handle non-zero VID/PID.
        if selector.vendor_id != 0 || selector.product_id != 0 {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        }

        let serial = selector.serial_number.as_deref().ok_or_else(|| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::ProbeSpecific(
                LinuxGpiodError::MissingSelector.into(),
            ))
        })?;

        let pins: PinMap = serial.parse().map_err(|e: LinuxGpiodError| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::ProbeSpecific(e.into()))
        })?;

        let probe = LinuxGpiod::open(&pins).map_err(|e| {
            DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::ProbeSpecific(e.into()))
        })?;

        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        // Pin assignments are board-specific; auto-discovery is not meaningful.
        Vec::new()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        // Synthesise an entry for any well-formed gpiod selector — the CLI
        // resolves `--probe` via this method before calling open().
        let Some(selector) = selector else {
            return Vec::new();
        };
        if selector.vendor_id != 0 || selector.product_id != 0 {
            return Vec::new();
        }
        let Some(serial) = selector.serial_number.as_deref() else {
            return Vec::new();
        };
        if serial.parse::<PinMap>().is_err() {
            return Vec::new();
        }
        vec![ProbeListItem::accessible(DebugProbeInfo::new(
            format!("Linux GPIO bit-bang SWD ({serial})"),
            0,
            0,
            Some(serial.to_string()),
            &LinuxGpiodFactory,
            None,
            false,
        ))]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: &str) -> DebugProbeSelector {
        s.parse().unwrap()
    }

    #[test]
    fn list_filtered_empty_with_no_selector() {
        assert!(LinuxGpiodFactory.list_probes_filtered(None).is_empty());
    }

    #[test]
    fn list_filtered_rejects_nonzero_vid() {
        let s = sel("1234:0:gpiochip0,swclk=1,swdio=2");
        assert!(LinuxGpiodFactory.list_probes_filtered(Some(&s)).is_empty());
    }

    #[test]
    fn list_filtered_rejects_nonzero_pid() {
        let s = sel("0:abcd:gpiochip0,swclk=1,swdio=2");
        assert!(LinuxGpiodFactory.list_probes_filtered(Some(&s)).is_empty());
    }

    #[test]
    fn list_filtered_rejects_no_serial() {
        let s = sel("0:0");
        assert!(LinuxGpiodFactory.list_probes_filtered(Some(&s)).is_empty());
    }

    #[test]
    fn list_filtered_rejects_unparsable_serial() {
        let s = sel("0:0:not-a-valid-pin-map");
        assert!(LinuxGpiodFactory.list_probes_filtered(Some(&s)).is_empty());
    }

    #[test]
    fn list_filtered_synthesises_entry_for_valid_selector() {
        let serial = "gpiochip1,swclk=26,swdio=25,srst=38";
        let s = sel(&format!("0:0:{serial}"));
        let entries = LinuxGpiodFactory.list_probes_filtered(Some(&s));
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].info.vendor_id, 0);
        assert_eq!(entries[0].info.product_id, 0);
        assert_eq!(entries[0].info.serial_number.as_deref(), Some(serial));
    }
}
