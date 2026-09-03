//! Chip variant, firmware gates and the clock tables.

use crate::probe::DebugProbeError;

use super::swd::SwdClock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Variant {
    Ch347F,
    Ch347T,
}

/// What the firmware version (`bcdDevice`) allows.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Capabilities {
    pub variant: Variant,
    pub firmware: u16,
}

impl Capabilities {
    /// Whether the fastest SWCLK (divisor 0, 5 MHz) is available. It was added in CH347F
    /// firmware 1.01 and CH347T firmware 5.44; earlier parts top out at 1 MHz.
    pub fn swd_5mhz(&self) -> bool {
        match self.variant {
            Variant::Ch347F => self.firmware >= 0x101,
            Variant::Ch347T => self.firmware >= 0x544,
        }
    }
}

/// How much one JTAG round may carry, which also selects the clock table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pack {
    Standard,
    Larger,
}

impl Pack {
    /// JTAG clock table in kHz, indexed by the value the init command takes.
    pub fn speeds(self) -> &'static [u32] {
        match self {
            Pack::Standard => &[1875, 3750, 7500, 15000, 30000, 60000],
            Pack::Larger => &[469, 938, 1875, 3750, 7500, 15000, 30000, 60000],
        }
    }
}

/// A resolved clock setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Clock {
    Jtag { index: u8, khz: u32 },
    Swd(SwdClock),
}

impl Clock {
    pub fn khz(self) -> u32 {
        match self {
            Clock::Jtag { khz, .. } => khz,
            Clock::Swd(clock) => clock.khz(),
        }
    }
}

/// Largest table entry at or below the request.
pub(super) fn jtag_clock(pack: Pack, khz: u32) -> Result<Clock, DebugProbeError> {
    pack.speeds()
        .iter()
        .enumerate()
        .rev()
        .find(|&(_, &speed)| speed <= khz)
        .map(|(index, &khz)| Clock::Jtag {
            index: index as u8,
            khz,
        })
        .ok_or(DebugProbeError::UnsupportedSpeed(khz))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jtag_speed_rounds_down_to_the_table() {
        assert_eq!(
            jtag_clock(Pack::Larger, 20000).unwrap(),
            Clock::Jtag {
                index: 5,
                khz: 15000
            }
        );
        assert_eq!(
            jtag_clock(Pack::Standard, 1875).unwrap(),
            Clock::Jtag {
                index: 0,
                khz: 1875
            }
        );
        assert_eq!(
            jtag_clock(Pack::Larger, 100_000).unwrap(),
            Clock::Jtag {
                index: 7,
                khz: 60000
            }
        );
        assert!(matches!(
            jtag_clock(Pack::Standard, 1000),
            Err(DebugProbeError::UnsupportedSpeed(1000))
        ));
    }
}
