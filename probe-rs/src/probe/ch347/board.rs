//! Board GPIO wiring. Supports the target reset line and an activity LED, opt-in by USB identity.
//! Without a known board, SWD drives the reset line on the otherwise unused TRST pin.

use std::num::NonZeroU8;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nusb::{DeviceInfo, MaybeFuture};

use crate::probe::{DebugProbeError, Pins, WireProtocol};

use super::capabilities::Variant;
use super::device::{Ch347Device, PID_CH347F, VID_WCH};
use super::transport::{Ch347Error, HEADER_LEN, TIMEOUT, frame};

const CMD_GPIO: u8 = 0xCC;
const GPIO_COUNT: usize = 8;
const PIN_NRESET: u8 = 1 << 7;
const RESET_PULSE: Duration = Duration::from_millis(50);

/// The LED during a session.
#[derive(Debug, Clone, Copy)]
pub(super) struct LedActivity {
    lit: bool,
    flipped: Instant,
}

/// A GPIO write touching one pin.
fn pin_block(pin: usize, drive: Drive) -> [u8; GPIO_COUNT] {
    let mut pins = [0; GPIO_COUNT];
    pins[pin] = drive.byte();
    pins
}

/// What a GPIO write does to one pin; every other pin byte is zero and left alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Drive {
    Low,
    High,
    /// Input, the output flag is cleared and the pin follows its pull-up.
    Input,
}

impl Drive {
    fn byte(self) -> u8 {
        match self {
            Drive::Low => 0xF0,
            Drive::High => 0xF8,
            Drive::Input => 0xC0,
        }
    }
}

/// A board's GPIO wiring, matched on the full USB identity. An unmatched device never gets a GPIO
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Board {
    pub name: &'static str,
    pub reset: Option<ResetPin>,
    pub led: Option<LedPin>,
}

/// The target's reset line on a GPIO, asserted low; the same over SWD and JTAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ResetPin {
    pub gpio: usize,
    /// `Input` for an open-drain style line with a pull-up on the target, `High` for a
    /// push-pull one.
    pub release: Drive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LedPin {
    pub gpio: usize,
    pub active_high: bool,
}

impl LedPin {
    fn drive(self, on: bool) -> Drive {
        if on == self.active_high {
            Drive::High
        } else {
            Drive::Low
        }
    }
}

/// The USB identity that selects a board, as written by its production tooling.
struct BoardId {
    vendor_id: u16,
    product_id: u16,
    manufacturer: &'static str,
    product: &'static str,
    board: Board,
}

/// Target RST on GPIO2, LED on GPIO0.
///
/// [Hardware design](https://github.com/korken89/wch-probe)
pub(crate) const WCH_PROBE: Board = Board {
    name: "wch-probe",
    reset: Some(ResetPin {
        gpio: 2,
        release: Drive::Input,
    }),
    led: Some(LedPin {
        gpio: 0,
        active_high: true,
    }),
};

const BOARDS: &[BoardId] = &[BoardId {
    vendor_id: VID_WCH,
    product_id: PID_CH347F,
    manufacturer: "korken89",
    product: "wch-probe",
    board: WCH_PROBE,
}];

impl Board {
    fn matching(
        vendor_id: u16,
        product_id: u16,
        manufacturer: Option<&str>,
        product: Option<&str>,
    ) -> Option<Board> {
        BOARDS
            .iter()
            .find(|id| {
                id.vendor_id == vendor_id
                    && id.product_id == product_id
                    && manufacturer == Some(id.manufacturer)
                    && product == Some(id.product)
            })
            .map(|id| id.board)
    }

    /// Enumeration may leave a string out or rename it, as Windows does with the
    /// manufacturer, so the device itself is asked first.
    pub(super) fn detect(device: &DeviceInfo, handle: &nusb::Device) -> Option<Board> {
        let descriptor = handle.device_descriptor();
        let language = handle
            .get_string_descriptor_supported_languages(TIMEOUT)
            .wait()
            .ok()
            .and_then(|mut languages| languages.next());
        let string = |index: Option<NonZeroU8>| {
            handle
                .get_string_descriptor(index?, language?, TIMEOUT)
                .wait()
                .ok()
        };
        let manufacturer = string(descriptor.manufacturer_string_index());
        let product = string(descriptor.product_string_index());
        Self::matching(
            device.vendor_id(),
            device.product_id(),
            manufacturer.as_deref().or(device.manufacturer_string()),
            product.as_deref().or(device.product_string()),
        )
    }
}

/// The chip's answer for one pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PinState {
    output: bool,
    high: bool,
}

impl PinState {
    fn from_byte(byte: u8) -> Self {
        Self {
            output: byte & 0x80 != 0,
            high: byte & 0x40 != 0,
        }
    }
}

/// SWD leaves the TRST pin free, so without a board it is the reset line. It is pin 9 on both
/// chips, but a different GPIO. Released to input, so the line needs a pull-up.
fn trst_reset_pin(variant: Variant) -> ResetPin {
    ResetPin {
        gpio: match variant {
            Variant::Ch347F => 3,
            Variant::Ch347T => 5,
        },
        release: Drive::Input,
    }
}

impl Ch347Device {
    /// A known board's wiring wins; without a board, the TRST pin while in SWD.
    fn reset_line(&self, protocol: WireProtocol) -> Option<ResetPin> {
        match self.board {
            Some(board) => board.reset,
            None => {
                (protocol == WireProtocol::Swd).then(|| trst_reset_pin(self.capabilities.variant))
            }
        }
    }

    fn reset_pin(&self) -> Result<ResetPin, DebugProbeError> {
        self.reset_line(self.protocol)
            .ok_or(Ch347Error::NoResetPin.into())
    }

    fn gpio_frame(
        &mut self,
        pins: [u8; GPIO_COUNT],
        pin: usize,
    ) -> Result<PinState, DebugProbeError> {
        let reply = self.command(CMD_GPIO, &pins, GPIO_COUNT)?;
        Ok(PinState::from_byte(reply[pin]))
    }

    /// A GPIO write for teardown.
    fn gpio_final(&mut self, pin: usize, drive: Drive) {
        if self
            .transport
            .write(&frame(CMD_GPIO, &pin_block(pin, drive)), TIMEOUT)
            .is_ok()
        {
            let _ = self
                .transport
                .read(&mut [0; HEADER_LEN + GPIO_COUNT], TIMEOUT);
        }
    }

    /// Drives one GPIO and returns what the chip reports for it.
    fn gpio(&mut self, pin: usize, drive: Drive) -> Result<PinState, DebugProbeError> {
        self.gpio_frame(pin_block(pin, drive), pin)
    }

    /// Pulls the target's reset line low or releases it the way the board wants. Returns
    /// the line's level afterwards.
    fn drive_reset(&mut self, pin: ResetPin, assert: bool) -> Result<bool, DebugProbeError> {
        let drive = if assert { Drive::Low } else { pin.release };
        // Recorded before the write, so a lost reply still gets the line released.
        if assert {
            self.reset_asserted = Some(pin);
        }
        let state = self.gpio(pin.gpio, drive)?;
        if state.output != (drive != Drive::Input) {
            return Err(Ch347Error::Reset(drive).into());
        }
        tracing::debug!(
            "target reset {}",
            if assert { "asserted" } else { "released" }
        );
        self.reset_asserted = assert.then_some(pin);
        Ok(state.high)
    }

    fn set_reset(&mut self, assert: bool) -> Result<(), DebugProbeError> {
        let pin = self.reset_pin()?;
        self.drive_reset(pin, assert).map(drop)
    }

    pub(crate) fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.set_reset(true)
    }

    pub(crate) fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.set_reset(false)
    }

    /// Releases the reset line at session start, whatever the protocol. The chip keeps GPIO
    /// state, so a session killed mid-reset leaves the target held.
    pub(super) fn release_reset(&mut self) -> Result<(), DebugProbeError> {
        if let Some(pin) = self.reset_line(self.protocol) {
            self.drive_reset(pin, false)?;
        }
        Ok(())
    }

    /// Hands the reset line over to `protocol`. A line that only exists in the old protocol is
    /// released rather than lost track of, and one that appears is released like at session
    /// start, as an earlier session may have left it low.
    pub(super) fn switch_reset_line(
        &mut self,
        protocol: WireProtocol,
    ) -> Result<(), DebugProbeError> {
        let old = self.reset_line(self.protocol);
        let new = self.reset_line(protocol);
        if old == new {
            return Ok(());
        }
        if let Some(pin) = old
            && self.reset_asserted.is_some()
        {
            self.drive_reset(pin, false)?;
        }
        if let Some(pin) = new {
            self.drive_reset(pin, false)?;
        }
        Ok(())
    }

    pub(crate) fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        let pin = self.reset_pin()?;
        self.drive_reset(pin, true)?;
        sleep(RESET_PULSE);
        self.drive_reset(pin, false).map(drop)
    }

    /// The CMSIS-DAP pin interface, for nRESET only.
    pub(crate) fn swj_pins(
        &mut self,
        out: Pins,
        select: Pins,
        wait: Duration,
    ) -> Result<(), DebugProbeError> {
        let unsupported = || DebugProbeError::CommandNotSupportedByProbe {
            command_name: "swj_pins",
        };
        let pin = self.reset_pin().map_err(|_| unsupported())?;
        if select.0 != PIN_NRESET {
            return Err(unsupported());
        }
        self.drive_reset(pin, !out.nreset())?;
        sleep(wait);
        Ok(())
    }

    /// Lights the board's LED for a session, or switches it off. While lit it blinks with
    /// traffic.
    pub(crate) fn set_led(&mut self, on: bool) -> Result<(), DebugProbeError> {
        let Some(led) = self.board.and_then(|board| board.led) else {
            return Ok(());
        };
        // Recorded before the write, so a lost reply still gets the LED switched off.
        if on {
            self.led = Some(LedActivity {
                lit: true,
                flipped: Instant::now(),
            });
        }
        self.gpio(led.gpio, led.drive(on))?;
        if !on {
            self.led = None;
        }
        Ok(())
    }

    /// Best effort flips the LED on traffic.
    pub(super) fn led_activity(&mut self, command: u8) {
        if command == CMD_GPIO {
            return;
        }
        let (Some(led), Some(activity)) = (self.board.and_then(|board| board.led), self.led) else {
            return;
        };
        let phase = if activity.lit {
            self.timing.led_on
        } else {
            self.timing.led_off
        };
        if activity.flipped.elapsed() < phase {
            return;
        }
        let lit = !activity.lit;
        match self.gpio(led.gpio, led.drive(lit)) {
            Ok(_) => {
                self.led = Some(LedActivity {
                    lit,
                    flipped: Instant::now(),
                });
            }
            Err(error) => tracing::warn!("LED blink failed: {error}"),
        }
    }
}

/// The chip keeps GPIO state across USB sessions, so a session that ends early must not
/// leave the target in reset or the LED lit.
impl Drop for Ch347Device {
    fn drop(&mut self) {
        if std::thread::panicking() {
            return;
        }
        if let Some(pin) = self.reset_asserted {
            self.gpio_final(pin.gpio, pin.release);
        }
        if self.led.is_some()
            && let Some(led) = self.board.and_then(|board| board.led)
        {
            self.gpio_final(led.gpio, led.drive(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::tests::{CH347F_1_20, PACK_PROBE, TRST_RELEASE, device, scripted};
    use super::*;
    use crate::probe::{BitSequence, WireProtocol};

    /// The wch-probe board, left in the default JTAG protocol to show reset works there.
    fn wch_probe(
        frames: &[(&[u8], &[u8])],
    ) -> (
        Ch347Device,
        std::sync::Arc<super::super::device::tests::Script>,
    ) {
        scripted(CH347F_1_20, Some(WCH_PROBE), frames)
    }

    // Reset is GPIO2, byte index 5 of the pin block.
    const RESET_LOW: &[u8] = &[0xCC, 8, 0, 0, 0, 0xF0, 0, 0, 0, 0, 0];
    const RESET_INPUT: &[u8] = &[0xCC, 8, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0];
    const PINS_RESET_LOW: &[u8] = &[0xCC, 8, 0, 0x40, 0x40, 0x80, 0x40, 0x40, 0x40, 0, 0];
    const PINS_RESET_HIGH: &[u8] = &[0xCC, 8, 0, 0x40, 0x40, 0x40, 0x40, 0x40, 0x40, 0, 0];

    // The LED is GPIO0, byte index 3 of the pin block.
    const LED_ON: &[u8] = &[0xCC, 8, 0, 0xF8, 0, 0, 0, 0, 0, 0, 0];
    const LED_OFF: &[u8] = &[0xCC, 8, 0, 0xF0, 0, 0, 0, 0, 0, 0, 0];
    const PINS_LED_ON: &[u8] = &[0xCC, 8, 0, 0xC0, 0x40, 0x40, 0x40, 0x40, 0x40, 0, 0];
    const PINS_LED_OFF: &[u8] = &[0xCC, 8, 0, 0x80, 0x40, 0x40, 0x40, 0x40, 0x40, 0, 0];

    #[test]
    fn reset_drives_low_and_releases_to_input() {
        // The last assert is answered with the pin still an input, which is an error, and
        // the line may be in any state, so drop releases it.
        let (mut dev, script) = wch_probe(&[
            (RESET_LOW, PINS_RESET_LOW),
            (RESET_INPUT, PINS_RESET_HIGH),
            (RESET_LOW, PINS_RESET_HIGH),
            (RESET_INPUT, PINS_RESET_HIGH),
        ]);
        dev.target_reset_assert().unwrap();
        dev.target_reset_deassert().unwrap();
        assert!(matches!(
            dev.target_reset_assert(),
            Err(DebugProbeError::ProbeSpecific(_))
        ));
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn swj_pins_maps_nreset_only() {
        let (mut dev, script) =
            wch_probe(&[(RESET_LOW, PINS_RESET_LOW), (RESET_INPUT, PINS_RESET_HIGH)]);
        let nreset = Pins(PIN_NRESET);
        dev.swj_pins(Pins(0), nreset, Duration::ZERO).unwrap();
        // A wait only pauses; nothing is sampled.
        dev.swj_pins(nreset, nreset, Duration::from_micros(1))
            .unwrap();
        assert!(script.finished());
        assert!(matches!(
            dev.swj_pins(Pins(0), Pins(PIN_NRESET | 1), Duration::ZERO),
            Err(DebugProbeError::CommandNotSupportedByProbe { .. })
        ));
    }

    #[test]
    fn release_reset_needs_a_board() {
        let (mut dev, script) = wch_probe(&[(RESET_INPUT, PINS_RESET_HIGH)]);
        dev.release_reset().unwrap();
        assert!(script.finished());

        let (mut dev, _) = device(&[]);
        dev.release_reset().unwrap();
    }

    // The generic CH347F's TRST pin is GPIO3, byte index 6 of the pin block.
    const TRST_LOW: (&[u8], &[u8]) = (
        &[0xCC, 8, 0, 0, 0, 0, 0xF0, 0, 0, 0, 0],
        &[0xCC, 8, 0, 0, 0, 0, 0x80, 0, 0, 0, 0],
    );

    /// A generic CH347F, switched to SWD.
    fn generic_swd(
        frames: &[(&[u8], &[u8])],
    ) -> (
        Ch347Device,
        std::sync::Arc<super::super::device::tests::Script>,
    ) {
        let frames: Vec<_> = std::iter::once(TRST_RELEASE)
            .chain(frames.iter().copied())
            .collect();
        let (mut dev, script) = device(&frames);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        (dev, script)
    }

    #[test]
    fn generic_reset_is_the_trst_pin_in_swd() {
        let (mut dev, script) = generic_swd(&[TRST_LOW, TRST_RELEASE]);
        dev.target_reset_assert().unwrap();
        dev.target_reset_deassert().unwrap();
        assert!(script.finished());
    }

    #[test]
    fn leaving_swd_releases_a_held_trst_reset() {
        let (mut dev, script) =
            generic_swd(&[TRST_LOW, TRST_RELEASE, (PACK_PROBE, &[0xD0, 1, 0, 1])]);
        dev.target_reset_assert().unwrap();
        dev.select_protocol(WireProtocol::Jtag).unwrap();
        // Over JTAG the pin is TRST, and the line is gone.
        assert!(matches!(
            dev.target_reset_assert(),
            Err(DebugProbeError::ProbeSpecific(_))
        ));
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn drop_releases_a_held_trst_reset() {
        let (mut dev, script) = generic_swd(&[TRST_LOW, TRST_RELEASE]);
        dev.target_reset_assert().unwrap();
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn a_board_overrides_the_trst_pin() {
        let (mut dev, script) =
            wch_probe(&[(RESET_LOW, PINS_RESET_LOW), (RESET_INPUT, PINS_RESET_HIGH)]);
        // The board's line is the same in both protocols, so switching leaves it alone.
        dev.select_protocol(WireProtocol::Swd).unwrap();
        dev.target_reset_assert().unwrap();
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn gpio_is_opt_in() {
        let (mut dev, _) = device(&[]);
        assert!(matches!(
            dev.target_reset_assert(),
            Err(DebugProbeError::ProbeSpecific(_))
        ));
        assert!(matches!(
            dev.swj_pins(Pins(0), Pins(PIN_NRESET), Duration::ZERO),
            Err(DebugProbeError::CommandNotSupportedByProbe { .. })
        ));
        dev.set_led(true).unwrap();
    }

    #[test]
    fn drop_releases_reset_and_led() {
        let (mut dev, script) = wch_probe(&[
            (RESET_LOW, PINS_RESET_LOW),
            (
                LED_ON,
                &[0xCC, 8, 0, 0xC0, 0x40, 0x80, 0x40, 0x40, 0x40, 0, 0],
            ),
            (RESET_INPUT, PINS_RESET_HIGH),
            (LED_OFF, PINS_LED_OFF),
        ]);
        dev.target_reset_assert().unwrap();
        dev.set_led(true).unwrap();
        drop(dev);
        assert!(script.finished());

        let (dev, script) = wch_probe(&[]);
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn drop_releases_reset_and_led_whose_replies_were_lost() {
        let (mut dev, script) = wch_probe(&[(RESET_LOW, &[]), (RESET_INPUT, PINS_RESET_HIGH)]);
        assert!(dev.target_reset_assert().is_err());
        drop(dev);
        assert!(script.finished());

        let (mut dev, script) = wch_probe(&[(LED_ON, &[]), (LED_OFF, PINS_LED_OFF)]);
        assert!(dev.set_led(true).is_err());
        drop(dev);
        assert!(script.finished());
    }

    #[test]
    fn boards_match_on_the_full_usb_identity() {
        let matching = |manufacturer, product| {
            Board::matching(0x1A86, 0x55DE, Some(manufacturer), Some(product))
        };
        assert_eq!(matching("korken89", "wch-probe"), Some(WCH_PROBE));
        assert_eq!(matching("Fresk", "wch-probe"), None);
        assert_eq!(matching("wch.cn", "wch-probe"), None);
        assert_eq!(matching("korken89", "UART+SPI+I2C+JTAG"), None);
        assert_eq!(
            Board::matching(0x1A86, 0x55DD, Some("korken89"), Some("wch-probe")),
            None
        );
        assert_eq!(Board::matching(0x1A86, 0x55DE, None, None), None);
    }

    #[test]
    fn led_is_on_for_a_session_and_blinks_with_traffic() {
        const SEQ: &[u8] = &[0xE8, 4, 0, 0xA1, 8, 0, 0];
        const SEQ_REPLY: &[u8] = &[0xE8, 1, 0, 0xA1];
        let (mut dev, script) = wch_probe(&[
            (LED_ON, PINS_LED_ON),
            (SEQ, SEQ_REPLY),
            (LED_OFF, PINS_LED_OFF),
            (SEQ, SEQ_REPLY),
            (LED_ON, PINS_LED_ON),
            (LED_OFF, PINS_LED_OFF),
        ]);
        dev.set_led(true).unwrap();
        dev.line_sequence(&BitSequence::repeat(false, 8)).unwrap();
        dev.line_sequence(&BitSequence::repeat(false, 8)).unwrap();
        dev.set_led(false).unwrap();
        assert!(script.finished());

        // Outside a session traffic leaves the LED alone.
        let (mut dev, script) = wch_probe(&[(SEQ, SEQ_REPLY)]);
        dev.line_sequence(&BitSequence::repeat(false, 8)).unwrap();
        assert!(script.finished());
    }

    #[test]
    fn drop_releases_reset_even_after_a_desync() {
        let init: &[u8] = &[0xE5, 8, 0, 0x40, 0x42, 0x0F, 0x00, 0, 0, 0, 0];
        let (mut dev, script) = wch_probe(&[
            (RESET_LOW, PINS_RESET_LOW),
            (init, &[0xD0, 1, 0, 0]),
            (RESET_INPUT, PINS_RESET_HIGH),
        ]);
        dev.target_reset_assert().unwrap();
        dev.select_protocol(WireProtocol::Swd).unwrap();
        assert!(dev.attach().is_err());
        drop(dev);
        assert!(script.finished());
    }
}
