use super::capabilities::Capability;
use super::error::JlinkError;

use super::Command;

use std::{cmp, fmt};

use super::JLink;

type Result<T> = std::result::Result<T, JlinkError>;

/// J-Link communication speed info.
#[derive(Debug)]
pub(super) struct SpeedInfo {
    pub base_freq: u32,
    pub min_div: u16,
}

impl SpeedInfo {
    /// Returns the maximum supported speed for target communication (in Hz).
    pub(crate) fn max_speed_hz(&self) -> u32 {
        self.base_freq / u32::from(self.min_div)
    }

    /// Returns a `SpeedConfig` that configures the fastest supported speed.
    #[expect(unused)]
    pub(crate) fn max_speed_config(&self) -> SpeedConfig {
        let khz = cmp::min(self.max_speed_hz() / 1000, 0xFFFE);
        // khz is guaranteed to be in the range 1..=0xFFFE, so let's skip the constructor
        SpeedConfig { raw: khz as u16 }
    }

    fn from_response(buf: [u8; 6]) -> SpeedInfo {
        let base_freq_bytes = <[u8; 4]>::try_from(&buf[0..4]).unwrap();
        let min_div_bytes = <[u8; 2]>::try_from(&buf[4..6]).unwrap();

        SpeedInfo {
            base_freq: u32::from_le_bytes(base_freq_bytes),
            min_div: u16::from_le_bytes(min_div_bytes),
        }
    }
}

/// Target communication speed setting.
///
/// This determines the clock frequency of the target communication. Supported speeds for the
/// currently selected target interface can be fetched via [`JLink::read_interface_speeds()`].
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct SpeedConfig {
    raw: u16,
}

impl SpeedConfig {
    /// Let the J-Link probe decide the speed.
    ///
    /// Requires the probe to support [`Capability::AdaptiveClocking`].
    pub const ADAPTIVE: Self = Self { raw: 0xFFFF };

    /// Manually specify speed in kHz.
    ///
    /// Returns `None` if the value is the invalid value `0xFFFF`. Note that this doesn't mean that
    /// every other value will be accepted by the device.
    pub(crate) fn khz(khz: u16) -> Option<Self> {
        if khz == SpeedConfig::ADAPTIVE.raw {
            None
        } else {
            Some(Self { raw: khz })
        }
    }
}

impl fmt::Display for SpeedConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if *self == Self::ADAPTIVE {
            f.write_str("adaptive")
        } else {
            write!(f, "{} kHz", self.raw)
        }
    }
}

impl JLink {
    /// Reads the probe's communication speed information about the currently selected interface.
    ///
    /// Supported speeds may differ between interfaces, so the right interface needs to be
    /// selected for the returned value to make sense.
    ///
    /// This requires the probe to support [`Capability::SpeedInfo`].
    pub(super) fn read_interface_speeds(&self) -> Result<SpeedInfo> {
        self.require_capability(Capability::SpeedInfo)?;

        self.write_cmd(&[Command::GetSpeeds as u8])?;

        let buf = self.read_n::<6>()?;

        Ok(SpeedInfo::from_response(buf))
    }

    /// Sets the target communication speed.
    ///
    /// If `speed` is set to [`SpeedConfig::ADAPTIVE`], then the probe has to support
    /// [`Capability::AdaptiveClocking`]. Note that adaptive clocking may not work for all target
    /// interfaces (eg. SWD).
    ///
    /// When the selected target interface is switched (by calling [`JLink::select_interface`], or
    /// any API method that automatically selects an interface), the communication speed is reset to
    /// some unspecified default value.
    pub(super) fn set_interface_clock_speed(&mut self, speed: SpeedConfig) -> Result<()> {
        if speed == SpeedConfig::ADAPTIVE {
            self.require_capability(Capability::AdaptiveClocking)?;
        }

        tracing::info!("Selecting speed: {} Hz", speed.raw);

        let [low, high] = speed.raw.to_le_bytes();
        self.write_cmd(&[Command::SetSpeed as u8, low, high])?;

        Ok(())
    }
}

#[cfg(test)]
mod test {
    #[test]
    fn test_speed_config() {
        let response = [0x00, 0x6C, 0xDC, 0x02, 0x04, 0x00];

        let speed_info = super::SpeedInfo::from_response(response);

        assert_eq!(speed_info.base_freq, 48_000_000);
        assert_eq!(speed_info.min_div, 4);
    }
}
