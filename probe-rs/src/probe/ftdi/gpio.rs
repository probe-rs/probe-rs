//! GPIO layout types for configurable FTDI probe pin assignments.
//!
//! This module provides a type-safe API for configuring FTDI GPIO pins.
//!
//! # Example
//!
//! ```
//! use probe_rs::probe::ftdi::{
//!     GpioState, FtdiPin, GpioSignal, ProbeLayout, SignalType, jtag_pins
//! };
//!
//! // Build a custom GPIO state
//! const STATE: GpioState = GpioState::new()
//!     .output_low(jtag_pins::TCK)
//!     .output_high(jtag_pins::TMS)
//!     .output_high(FtdiPin::Acbus0); // nTRST
//!
//! // Define signals for the layout
//! static SIGNALS: &[GpioSignal] = &[
//!     GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0),
//!     GpioSignal::new(SignalType::Srst, FtdiPin::Acbus1),
//!     GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3),
//! ];
//!
//! // Create a complete probe layout
//! static MY_LAYOUT: ProbeLayout = ProbeLayout::new("My Probe", STATE, SIGNALS);
//! ```

/// Represents individual FTDI GPIO pins.
///
/// FTDI chips have 16 GPIO pins split across two buses:
/// - ADBUS0-7: Low byte (addressable via 0x80 MPSSE command)
/// - ACBUS0-7: High byte (addressable via 0x82 MPSSE command)
///
/// Note: ADBUS0-3 are typically hardwired for JTAG (TCK, TDI, TDO, TMS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtdiPin {
    /// ADBUS0 - typically TCK in JTAG mode
    Adbus0,
    /// ADBUS1 - typically TDI in JTAG mode
    Adbus1,
    /// ADBUS2 - typically TDO in JTAG mode
    Adbus2,
    /// ADBUS3 - typically TMS in JTAG mode
    Adbus3,
    /// ADBUS4 - general purpose I/O
    Adbus4,
    /// ADBUS5 - general purpose I/O
    Adbus5,
    /// ADBUS6 - general purpose I/O
    Adbus6,
    /// ADBUS7 - general purpose I/O
    Adbus7,
    /// ACBUS0 - typically nTRST
    Acbus0,
    /// ACBUS1 - typically nSRST
    Acbus1,
    /// ACBUS2 - general purpose I/O
    Acbus2,
    /// ACBUS3 - general purpose I/O
    Acbus3,
    /// ACBUS4 - general purpose I/O
    Acbus4,
    /// ACBUS5 - general purpose I/O
    Acbus5,
    /// ACBUS6 - general purpose I/O
    Acbus6,
    /// ACBUS7 - general purpose I/O
    Acbus7,
}

impl FtdiPin {
    /// Returns the 16-bit mask for this pin.
    pub const fn mask(self) -> u16 {
        match self {
            FtdiPin::Adbus0 => 1 << 0,
            FtdiPin::Adbus1 => 1 << 1,
            FtdiPin::Adbus2 => 1 << 2,
            FtdiPin::Adbus3 => 1 << 3,
            FtdiPin::Adbus4 => 1 << 4,
            FtdiPin::Adbus5 => 1 << 5,
            FtdiPin::Adbus6 => 1 << 6,
            FtdiPin::Adbus7 => 1 << 7,
            FtdiPin::Acbus0 => 1 << 8,
            FtdiPin::Acbus1 => 1 << 9,
            FtdiPin::Acbus2 => 1 << 10,
            FtdiPin::Acbus3 => 1 << 11,
            FtdiPin::Acbus4 => 1 << 12,
            FtdiPin::Acbus5 => 1 << 13,
            FtdiPin::Acbus6 => 1 << 14,
            FtdiPin::Acbus7 => 1 << 15,
        }
    }
}

/// Standard JTAG pin assignments on FTDI chips.
pub mod jtag_pins {
    use super::FtdiPin;

    /// TCK - JTAG clock (ADBUS0)
    pub const TCK: FtdiPin = FtdiPin::Adbus0;
    /// TDI - JTAG data in (ADBUS1)
    pub const TDI: FtdiPin = FtdiPin::Adbus1;
    /// TDO - JTAG data out (ADBUS2)
    pub const TDO: FtdiPin = FtdiPin::Adbus2;
    /// TMS - JTAG mode select (ADBUS3)
    pub const TMS: FtdiPin = FtdiPin::Adbus3;
}

/// Represents the state of all 16 GPIO pins (data levels and directions).
///
/// This struct tracks:
/// - `data`: Output levels (1 = high, 0 = low) for output pins
/// - `direction`: Pin direction (1 = output, 0 = input)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GpioState {
    /// Output data levels (1 = high, 0 = low)
    data: u16,
    /// Pin directions (1 = output, 0 = input)
    direction: u16,
}

impl GpioState {
    /// Creates a new GpioState with all pins as inputs at low level.
    pub const fn new() -> Self {
        Self {
            data: 0,
            direction: 0,
        }
    }

    /// Creates a GpioState from raw values.
    pub const fn from_raw(data: u16, direction: u16) -> Self {
        Self { data, direction }
    }

    /// Configures a pin as an output driving high.
    pub const fn output_high(self, pin: FtdiPin) -> Self {
        let mask = pin.mask();
        Self {
            data: self.data | mask,
            direction: self.direction | mask,
        }
    }

    /// Configures a pin as an output driving low.
    pub const fn output_low(self, pin: FtdiPin) -> Self {
        let mask = pin.mask();
        Self {
            data: self.data & !mask,
            direction: self.direction | mask,
        }
    }

    /// Configures a pin as an input.
    pub const fn input(self, pin: FtdiPin) -> Self {
        let mask = pin.mask();
        Self {
            data: self.data,
            direction: self.direction & !mask,
        }
    }

    /// Sets a pin high (must already be configured as output).
    pub fn set_high(&mut self, pin: FtdiPin) {
        self.data |= pin.mask();
    }

    /// Sets a pin low (must already be configured as output).
    pub fn set_low(&mut self, pin: FtdiPin) {
        self.data &= !pin.mask();
    }

    /// Returns the low byte (ADBUS) as (data, direction).
    pub const fn low_byte(&self) -> (u8, u8) {
        (self.data as u8, self.direction as u8)
    }

    /// Returns the high byte (ACBUS) as (data, direction).
    pub const fn high_byte(&self) -> (u8, u8) {
        ((self.data >> 8) as u8, (self.direction >> 8) as u8)
    }

    /// Returns the full 16-bit values as (data, direction).
    pub const fn as_u16(&self) -> (u16, u16) {
        (self.data, self.direction)
    }
}

impl std::fmt::Display for GpioState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "layout_init 0x{:04x} 0x{:04x}",
            self.data, self.direction
        )
    }
}

/// Signal types that can be mapped to GPIO pins.
///
/// Standard signals (TRST, SRST) have well-known semantics and are typically
/// used by the probe infrastructure. Custom signals can be defined for
/// probe-specific functionality like LEDs or enable pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalType {
    /// JTAG Test Reset - resets the JTAG TAP state machine.
    /// Typically active-low (nTRST).
    Trst,
    /// System Reset - resets the target microcontroller.
    /// Typically active-low (nSRST).
    Srst,
    /// Custom signal with a name (e.g., "LED", "JCOMP").
    Custom(&'static str),
}

/// Defines a GPIO signal mapping to physical pins.
///
/// This maps logical signals to physical GPIO pins, similar to
/// OpenOCD's `ftdi layout_signal` command.
#[derive(Debug, Clone, Copy)]
pub struct GpioSignal {
    /// The type of signal (standard or custom).
    pub signal: SignalType,
    /// Mask for the data pin(s) controlling this signal.
    pub data_mask: u16,
    /// Mask for the output enable pin(s) - usually same as data_mask.
    pub oe_mask: u16,
    /// If true, the signal is active when driven low.
    pub active_low: bool,
}

impl GpioSignal {
    /// Creates a new signal (active-low) on a single pin.
    ///
    /// Use this for signals like TRST and SRST which are typically active-low.
    pub const fn new(signal: SignalType, pin: FtdiPin) -> Self {
        let mask = pin.mask();
        Self {
            signal,
            data_mask: mask,
            oe_mask: mask,
            active_low: true,
        }
    }

    /// Creates a new signal with active-high polarity.
    ///
    /// Use this for signals like LED or JCOMP that are asserted when driven high.
    pub const fn new_active_high(signal: SignalType, pin: FtdiPin) -> Self {
        let mask = pin.mask();
        Self {
            signal,
            data_mask: mask,
            oe_mask: mask,
            active_low: false,
        }
    }

    /// Creates a signal with custom data and output-enable masks.
    pub const fn with_masks(
        signal: SignalType,
        data_mask: u16,
        oe_mask: u16,
        active_low: bool,
    ) -> Self {
        Self {
            signal,
            data_mask,
            oe_mask,
            active_low,
        }
    }

    /// Applies the "asserted" state of this signal to a GpioState.
    pub fn assert(&self, state: &mut GpioState) {
        state.direction |= self.oe_mask;
        if self.active_low {
            state.data &= !self.data_mask;
        } else {
            state.data |= self.data_mask;
        }
    }

    /// Applies the "deasserted" state of this signal to a GpioState.
    pub fn deassert(&self, state: &mut GpioState) {
        state.direction |= self.oe_mask;
        if self.active_low {
            state.data |= self.data_mask;
        } else {
            state.data &= !self.data_mask;
        }
    }
}

/// Complete GPIO configuration for a probe type.
///
/// This bundles together:
/// - A descriptive name for the probe/layout
/// - The initial GPIO state at attach time
/// - A list of named signals available on this probe
#[derive(Debug, Clone, Copy)]
pub struct ProbeLayout {
    /// Descriptive name for this layout (e.g., "Olimex ARM-USB-TINY-H")
    pub name: &'static str,
    /// Initial GPIO state applied during attach
    pub init_state: GpioState,
    /// Available named signals on this probe
    pub signals: &'static [GpioSignal],
}

impl ProbeLayout {
    /// Creates a new probe layout.
    pub const fn new(
        name: &'static str,
        init_state: GpioState,
        signals: &'static [GpioSignal],
    ) -> Self {
        Self {
            name,
            init_state,
            signals,
        }
    }

    /// Looks up a signal by type.
    pub fn signal(&self, signal_type: SignalType) -> Option<&GpioSignal> {
        self.signals.iter().find(|s| s.signal == signal_type)
    }
}

// ============================================================================
// Predefined Layouts
// ============================================================================

/// Base JTAG pin configuration.
///
/// - TCK: output low
/// - TDI: output low
/// - TDO: input
/// - TMS: output high (idle state)
pub const JTAG_PINS: GpioState = GpioState::new()
    .output_low(jtag_pins::TCK)
    .output_low(jtag_pins::TDI)
    .input(jtag_pins::TDO)
    .output_high(jtag_pins::TMS);

/// Generic FTDI layout with only basic JTAG pins.
///
/// Use this for unknown FTDI devices or devices without reset signals.
pub const GENERIC_FTDI: ProbeLayout = ProbeLayout::new("Generic FTDI", JTAG_PINS, &[]);

/// Olimex ARM-USB-TINY-H layout.
///
/// Pin assignments:
/// - ACBUS0: nTRST (directly connected)
/// - ACBUS1: nSRST (directly connected)
/// - ACBUS3: LED
///
/// Initial state: nTRST high (deasserted), nSRST high (deasserted), LED on
pub const OLIMEX_ARM_USB_TINY_H: ProbeLayout = ProbeLayout::new(
    "Olimex ARM-USB-TINY-H",
    JTAG_PINS
        .output_high(FtdiPin::Acbus0)
        .output_high(FtdiPin::Acbus1)
        .output_high(FtdiPin::Acbus3),
    &[
        GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0),
        GpioSignal::new(SignalType::Srst, FtdiPin::Acbus1),
        GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3),
    ],
);

/// Olimex ARM-USB-OCD-H layout.
///
/// Same pin assignments as ARM-USB-TINY-H.
pub const OLIMEX_ARM_USB_OCD_H: ProbeLayout = ProbeLayout::new(
    "Olimex ARM-USB-OCD-H",
    JTAG_PINS
        .output_high(FtdiPin::Acbus0)
        .output_high(FtdiPin::Acbus1)
        .output_high(FtdiPin::Acbus3),
    &[
        GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0),
        GpioSignal::new(SignalType::Srst, FtdiPin::Acbus1),
        GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3),
    ],
);

/// Olimex ARM-USB-TINY layout (older, non-H variant).
///
/// Same pin assignments as the -H variant.
pub const OLIMEX_ARM_USB_TINY: ProbeLayout = ProbeLayout::new(
    "Olimex ARM-USB-TINY",
    JTAG_PINS
        .output_high(FtdiPin::Acbus0)
        .output_high(FtdiPin::Acbus1)
        .output_high(FtdiPin::Acbus3),
    &[
        GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0),
        GpioSignal::new(SignalType::Srst, FtdiPin::Acbus1),
        GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3),
    ],
);

/// Olimex ARM-USB-OCD layout (older, non-H variant).
///
/// Same pin assignments as the -H variant.
pub const OLIMEX_ARM_USB_OCD: ProbeLayout = ProbeLayout::new(
    "Olimex ARM-USB-OCD",
    JTAG_PINS
        .output_high(FtdiPin::Acbus0)
        .output_high(FtdiPin::Acbus1)
        .output_high(FtdiPin::Acbus3),
    &[
        GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0),
        GpioSignal::new(SignalType::Srst, FtdiPin::Acbus1),
        GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3),
    ],
);

/// Digilent HS1 layout.
pub const DIGILENT_HS1: ProbeLayout = ProbeLayout::new(
    "Digilent HS1",
    JTAG_PINS.output_high(FtdiPin::Adbus7),
    &[],
);

/// Digilent HS2 layout.
pub const DIGILENT_HS2: ProbeLayout = ProbeLayout::new(
    "Digilent HS2",
    JTAG_PINS
        .output_high(FtdiPin::Adbus5)
        .output_high(FtdiPin::Adbus6)
        .output_high(FtdiPin::Adbus7)
        .output_low(FtdiPin::Acbus5)
        .output_low(FtdiPin::Acbus6),
    &[],
);

/// Digilent HS3 layout.
pub const DIGILENT_HS3: ProbeLayout = ProbeLayout::new(
    "Digilent HS3",
    JTAG_PINS
        .output_high(FtdiPin::Adbus7)
        .output_low(FtdiPin::Acbus4)
        .output_high(FtdiPin::Acbus5),
    &[],
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pin_masks() {
        assert_eq!(FtdiPin::Adbus0.mask(), 0x0001);
        assert_eq!(FtdiPin::Adbus7.mask(), 0x0080);
        assert_eq!(FtdiPin::Acbus0.mask(), 0x0100);
        assert_eq!(FtdiPin::Acbus7.mask(), 0x8000);
    }

    #[test]
    fn test_gpio_state_builder() {
        let state = GpioState::new()
            .output_high(jtag_pins::TMS)
            .output_low(jtag_pins::TCK)
            .input(jtag_pins::TDO);

        let (data, dir) = state.as_u16();
        assert_eq!(data, 0x0008);
        assert_eq!(dir, 0x0009);
    }

    #[test]
    fn test_gpio_state_bytes() {
        let state = GpioState::from_raw(0x1234, 0x5678);
        assert_eq!(state.low_byte(), (0x34, 0x78));
        assert_eq!(state.high_byte(), (0x12, 0x56));
    }

    #[test]
    fn test_signal_assert_active_low() {
        let signal = GpioSignal::new(SignalType::Trst, FtdiPin::Acbus0);
        let mut state = GpioState::from_raw(0x0100, 0x0100); // Initially high

        signal.assert(&mut state);
        assert_eq!(state.data & 0x0100, 0); // Should be low (asserted)

        signal.deassert(&mut state);
        assert_eq!(state.data & 0x0100, 0x0100); // Should be high (deasserted)
    }

    #[test]
    fn test_signal_assert_active_high() {
        let signal = GpioSignal::new_active_high(SignalType::Custom("LED"), FtdiPin::Acbus3);
        let mut state = GpioState::from_raw(0x0000, 0x0800); // Initially low

        signal.assert(&mut state);
        assert_eq!(state.data & 0x0800, 0x0800); // Should be high (asserted)

        signal.deassert(&mut state);
        assert_eq!(state.data & 0x0800, 0); // Should be low (deasserted)
    }

    #[test]
    fn test_jtag_pins_layout() {
        let (data, dir) = JTAG_PINS.as_u16();
        assert_eq!(data, 0x0008);
        assert_eq!(dir, 0x000b);
    }

    #[test]
    fn test_olimex_layout() {
        let (data, dir) = OLIMEX_ARM_USB_TINY_H.init_state.as_u16();
        assert_eq!(data, 0x0b08);
        assert_eq!(dir, 0x0b0b);

        assert!(OLIMEX_ARM_USB_TINY_H.signal(SignalType::Trst).is_some());
        assert!(OLIMEX_ARM_USB_TINY_H.signal(SignalType::Srst).is_some());
        assert!(OLIMEX_ARM_USB_TINY_H
            .signal(SignalType::Custom("LED"))
            .is_some());
        assert!(OLIMEX_ARM_USB_TINY_H
            .signal(SignalType::Custom("nonexistent"))
            .is_none());
    }

    #[test]
    fn test_digilent_layouts_match_original() {
        assert_eq!(DIGILENT_HS1.init_state.as_u16(), (0x0088, 0x008b));
        assert_eq!(DIGILENT_HS2.init_state.as_u16(), (0x00e8, 0x60eb));
        assert_eq!(DIGILENT_HS3.init_state.as_u16(), (0x2088, 0x308b));
    }

    #[test]
    fn test_generic_layout_matches_default() {
        assert_eq!(GENERIC_FTDI.init_state.as_u16(), (0x0008, 0x000b));
    }
}
