//! FTDI-based debug probes.

pub use gpio::{FtdiPin, GpioSignal, GpioState, ProbeLayout, SignalType, jtag_pins};

use crate::{
    architecture::{
        arm::{
            ArmCommunicationInterface, ArmDebugInterface, ArmError,
            communication_interface::DapProbe, sequences::ArmDebugSequence,
        },
        riscv::{
            communication_interface::{RiscvError, RiscvInterfaceBuilder},
            dtm::jtag_dtm::JtagDtmBuilder,
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState, XtensaError,
        },
    },
    probe::{
        AutoImplementJtagAccess, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
        IoSequenceItem, JtagAccess, JtagDriverState, ProbeCreationError, ProbeFactory,
        ProbeStatistics, RawJtagIo, RawSwdIo, SwdSettings, WireProtocol,
    },
};
use bitvec::prelude::*;
use nusb::{DeviceInfo, MaybeFuture};
use std::{
    io::{Read, Write},
    sync::Arc,
    time::{Duration, Instant},
};

mod command_compacter;
mod ftdaye;
pub mod gpio;

use command_compacter::Command;
use ftdaye::{ChipType, error::FtdiError};

#[derive(Debug)]
struct JtagAdapter {
    /// USB device handle for the FTDI chip.
    device: ftdaye::Device,
    /// Current JTAG clock speed in kHz.
    speed_khz: u32,

    /// Current MPSSE command being built.
    command: Command,
    /// Buffer of encoded MPSSE commands to send.
    commands: Vec<u8>,
    /// Number of bits to read for each response byte.
    in_bit_counts: Vec<usize>,
    /// Captured TDO bits from JTAG operations.
    in_bits: BitVec,
    /// FTDI chip properties (buffer size, max clock, etc.).
    ftdi: FtdiProperties,

    /// GPIO layout for this adapter.
    layout: &'static ProbeLayout,
    /// Current GPIO state (tracks pin levels and directions).
    gpio_state: GpioState,
}

impl JtagAdapter {
    fn open(ftdi: FtdiDevice, usb_device: DeviceInfo) -> Result<Self, DebugProbeError> {
        let device = ftdaye::Builder::new()
            .with_interface(ftdaye::Interface::A)
            .with_read_timeout(Duration::from_secs(5))
            .with_write_timeout(Duration::from_secs(5))
            .usb_open(usb_device)?;

        let ftdi_props = FtdiProperties::try_from((ftdi, device.chip_type()))?;

        Ok(Self {
            device,
            speed_khz: 1000,
            command: Command::default(),
            commands: vec![],
            in_bit_counts: vec![],
            in_bits: BitVec::new(),
            ftdi: ftdi_props,
            layout: ftdi.layout,
            gpio_state: ftdi.layout.init_state,
        })
    }

    pub fn attach(&mut self) -> Result<(), FtdiError> {
        self.device.usb_reset()?;
        // 0x0B configures pins for JTAG
        self.device.set_bitmode(0x0b, ftdaye::BitMode::Mpsse)?;
        self.device.set_latency_timer(1)?;
        self.device.usb_purge_buffers()?;

        let mut junk = vec![];
        let _ = self.device.read_to_end(&mut junk);

        self.select_layout_by_product_string();
        self.gpio_state = self.layout.init_state;
        let (output, direction) = self.gpio_state.as_u16();
        self.device.set_pins(output, direction)?;

        self.apply_clock_speed(self.speed_khz)?;

        self.device.disable_loopback()?;

        Ok(())
    }

    /// Selects the appropriate layout based on USB product string.
    ///
    /// This is needed for Digilent devices that use generic FTDI VID/PID pairs
    /// but have different pin assignments.
    fn select_layout_by_product_string(&mut self) {
        let layout = match (
            self.device.vendor_id(),
            self.device.product_id(),
            self.device.product_string().unwrap_or(""),
        ) {
            // Digilent HS3
            (0x0403, 0x6014, "Digilent USB Device") => &gpio::DIGILENT_HS3,
            // Digilent HS2
            (0x0403, 0x6014, "Digilent Adept USB Device") => &gpio::DIGILENT_HS2,
            // Digilent HS1
            (0x0403, 0x6010, "Digilent Adept USB Device") => &gpio::DIGILENT_HS1,
            // Built-in Digilent HS1 (on-board)
            (0x0403, 0x6010, "Digilent USB Device") => &gpio::DIGILENT_HS1,
            _ => return,
        };
        self.layout = layout;
    }

    /// Asserts a signal (drives it to its active state).
    ///
    /// Returns `Ok(())` if the signal was found and asserted, or
    /// `Err` if the signal doesn't exist in the current layout.
    fn assert_signal(&mut self, signal: SignalType) -> Result<(), FtdiError> {
        let signal_def = self.layout.signal(signal).ok_or_else(|| {
            FtdiError::Other(format!("Signal '{:?}' not found in layout", signal))
        })?;

        signal_def.assert(&mut self.gpio_state);
        let (output, direction) = self.gpio_state.as_u16();
        self.device.set_pins(output, direction)?;
        Ok(())
    }

    /// Deasserts a signal (drives it to its inactive state).
    ///
    /// Returns `Ok(())` if the signal was found and deasserted, or
    /// `Err` if the signal doesn't exist in the current layout.
    fn deassert_signal(&mut self, signal: SignalType) -> Result<(), FtdiError> {
        let signal_def = self.layout.signal(signal).ok_or_else(|| {
            FtdiError::Other(format!("Signal '{:?}' not found in layout", signal))
        })?;

        signal_def.deassert(&mut self.gpio_state);
        let (output, direction) = self.gpio_state.as_u16();
        self.device.set_pins(output, direction)?;
        Ok(())
    }

    /// Checks if a signal is available in the current layout.
    fn has_signal(&self, signal: SignalType) -> bool {
        self.layout.signal(signal).is_some()
    }

    /// Resets the JTAG TAP state machine.
    ///
    /// If TRST signal is available, pulses it. Otherwise, clocks TMS=1
    /// for 5 cycles to reach Test-Logic-Reset state.
    fn jtag_reset(&mut self) -> Result<(), DebugProbeError> {
        if self.has_signal(SignalType::Trst) {
            self.assert_signal(SignalType::Trst)?;
            std::thread::sleep(Duration::from_millis(10));
            self.deassert_signal(SignalType::Trst)?;
        } else {
            for _ in 0..5 {
                self.shift_bit(true, false, false)?;
            }
            self.flush()?;
        }
        Ok(())
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed_khz(&mut self, speed_khz: u32) -> u32 {
        self.speed_khz = speed_khz;
        self.speed_khz
    }

    fn apply_clock_speed(&mut self, speed_khz: u32) -> Result<u32, FtdiError> {
        // Disable divide-by-5 mode if available
        if self.ftdi.has_divide_by_5 {
            self.device.disable_divide_by_5()?;
        } else {
            // Force enable divide-by-5 mode if not available or unknown
            self.device.enable_divide_by_5()?;
        }

        // If `speed_khz` is not a divisor of the maximum supported speed, we need to round up
        let is_exact = self.ftdi.max_clock.is_multiple_of(speed_khz);

        // If `speed_khz` is 0, use the maximum supported speed
        let divisor =
            (self.ftdi.max_clock.checked_div(speed_khz).unwrap_or(1) - is_exact as u32).min(0xFFFF);

        let actual_speed = self.ftdi.max_clock / (divisor + 1);

        tracing::info!(
            "Setting speed to {} kHz (divisor: {}, actual speed: {} kHz)",
            speed_khz,
            divisor,
            actual_speed
        );

        self.device.configure_clock_divider(divisor as u16)?;

        self.speed_khz = actual_speed;
        Ok(actual_speed)
    }

    fn read_response(&mut self) -> Result<(), DebugProbeError> {
        if self.in_bit_counts.is_empty() {
            return Ok(());
        }

        let mut t0 = Instant::now();
        let timeout = Duration::from_millis(10);

        let mut reply = Vec::with_capacity(self.in_bit_counts.len());
        while reply.len() < self.in_bit_counts.len() {
            let read = self
                .device
                .read_to_end(&mut reply)
                .map_err(FtdiError::from)?;

            if read > 0 {
                t0 = Instant::now();
            }

            if t0.elapsed() > timeout {
                tracing::warn!(
                    "Read {} bytes, expected {}",
                    reply.len(),
                    self.in_bit_counts.len()
                );
                return Err(DebugProbeError::Timeout);
            }
        }

        if reply.len() != self.in_bit_counts.len() {
            return Err(DebugProbeError::Other(format!(
                "Read more data than expected. Expected {} bytes, got {} bytes",
                self.in_bit_counts.len(),
                reply.len()
            )));
        }

        for (byte, count) in reply.into_iter().zip(self.in_bit_counts.drain(..)) {
            let bits = byte >> (8 - count);
            self.in_bits
                .extend_from_bitslice(&bits.view_bits::<Lsb0>()[..count]);
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.finalize_command()?;
        self.send_buffer()?;
        self.read_response()?;

        Ok(())
    }

    fn append_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        tracing::trace!("Appending {:?}", command);
        // 1 byte is reserved for the send immediate command
        if self.commands.len() + command.len() + 1 >= self.ftdi.buffer_size {
            self.send_buffer()?;
            self.read_response()?;
        }

        command.add_captured_bits(&mut self.in_bit_counts);
        command.encode(&mut self.commands);

        Ok(())
    }

    fn finalize_command(&mut self) -> Result<(), DebugProbeError> {
        if let Some(command) = self.command.take() {
            self.append_command(command)?;
        }

        Ok(())
    }

    fn shift_bit(&mut self, tms: bool, tdi: bool, capture: bool) -> Result<(), DebugProbeError> {
        if let Some(command) = self.command.append_jtag_bit(tms, tdi, capture) {
            self.append_command(command)?;
        }

        Ok(())
    }

    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        if self.commands.is_empty() {
            return Ok(());
        }

        // Send Immediate: This will make the FTDI chip flush its buffer back to the PC.
        // See https://www.ftdichip.com/Support/Documents/AppNotes/AN_108_Command_Processor_for_MPSSE_and_MCU_Host_Bus_Emulation_Modes.pdf
        // section 5.1
        self.commands.push(0x87);

        tracing::trace!("Sending buffer: {:X?}", self.commands);

        self.device
            .write_all(&self.commands)
            .map_err(FtdiError::from)?;

        self.commands.clear();

        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush()?;

        Ok(std::mem::take(&mut self.in_bits))
    }
}

/// A factory for creating [`FtdiProbe`] instances.
#[derive(Debug)]
pub struct FtdiProbeFactory;

impl std::fmt::Display for FtdiProbeFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FTDI")
    }
}

impl ProbeFactory for FtdiProbeFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        // Only open FTDI-compatible probes
        let Some(ftdi) = FTDI_COMPAT_DEVICES
            .iter()
            .find(|ftdi| ftdi.id == (selector.vendor_id, selector.product_id))
            .copied()
        else {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        };

        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| DebugProbeError::from(FtdiError::Usb(e.into())))?;

        let mut probes = devices
            .filter(|usb_info| selector.matches(usb_info))
            .collect::<Vec<_>>();

        if probes.is_empty() {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        } else if probes.len() > 1 {
            tracing::warn!("More than one matching FTDI probe was found. Opening the first one.");
        }

        let probe = FtdiProbe {
            adapter: JtagAdapter::open(ftdi, probes.pop().unwrap())?,
            jtag_state: JtagDriverState::default(),
            swd_settings: SwdSettings::default(),
            probe_statistics: ProbeStatistics::default(),
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_ftdi_devices()
    }
}

/// An FTDI-based debug probe.
#[derive(Debug)]
pub struct FtdiProbe {
    adapter: JtagAdapter,
    jtag_state: JtagDriverState,
    probe_statistics: ProbeStatistics,
    swd_settings: SwdSettings,
}

impl FtdiProbe {
    /// Resets the JTAG TAP state machine.
    ///
    /// If the probe has a TRST signal, it will be pulsed. Otherwise,
    /// TMS=1 is clocked for 5 cycles to reach Test-Logic-Reset state.
    pub fn jtag_reset(&mut self) -> Result<(), DebugProbeError> {
        self.adapter.jtag_reset()
    }

    /// Returns true if this probe has a hardware TRST signal available.
    pub fn has_trst(&self) -> bool {
        self.adapter.has_signal(SignalType::Trst)
    }
}

impl DebugProbe for FtdiProbe {
    fn get_name(&self) -> &str {
        "FTDI"
    }

    fn speed_khz(&self) -> u32 {
        self.adapter.speed_khz()
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        Ok(self.adapter.set_speed_khz(speed_khz))
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching...");

        self.adapter.attach()?;
        self.select_target(0)
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        // Check if this probe has an SRST signal
        if !self.adapter.has_signal(SignalType::Srst) {
            return Err(DebugProbeError::NotImplemented {
                function_name: "target_reset",
            });
        }

        self.target_reset_assert()?;
        std::thread::sleep(Duration::from_millis(100));
        self.target_reset_deassert()?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.adapter
            .assert_signal(SignalType::Srst)
            .map_err(|e| DebugProbeError::Other(e.to_string()))
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.adapter
            .deassert_signal(SignalType::Srst)
            .map_err(|e| DebugProbeError::Other(e.to_string()))
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if protocol != WireProtocol::Jtag {
            Err(DebugProbeError::UnsupportedProtocol(protocol))
        } else {
            Ok(())
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        // Only supports JTAG
        Some(WireProtocol::Jtag)
    }

    fn try_as_jtag_probe(&mut self) -> Option<&mut dyn JtagAccess> {
        Some(self)
    }

    fn try_get_riscv_interface_builder<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvInterfaceBuilder<'probe> + 'probe>, RiscvError> {
        Ok(Box::new(JtagDtmBuilder::new(self)))
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        Ok(ArmCommunicationInterface::create(self, sequence, true))
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut XtensaDebugInterfaceState,
    ) -> Result<XtensaCommunicationInterface<'probe>, XtensaError> {
        Ok(XtensaCommunicationInterface::new(self, state))
    }

    fn has_xtensa_interface(&self) -> bool {
        true
    }
}

impl AutoImplementJtagAccess for FtdiProbe {}
impl DapProbe for FtdiProbe {}

impl RawSwdIo for FtdiProbe {
    fn swd_io<S>(&mut self, _swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
        Err(DebugProbeError::NotImplemented {
            function_name: "swd_io",
        })
    }

    fn swj_pins(
        &mut self,
        _pin_out: u32,
        _pin_select: u32,
        _pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "swj_pins",
        })
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }

    fn probe_statistics(&mut self) -> &mut ProbeStatistics {
        &mut self.probe_statistics
    }
}

impl RawJtagIo for FtdiProbe {
    fn shift_bit(
        &mut self,
        tms: bool,
        tdi: bool,
        capture_tdo: bool,
    ) -> Result<(), DebugProbeError> {
        self.jtag_state.state.update(tms);
        self.adapter.shift_bit(tms, tdi, capture_tdo)?;
        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.adapter.read_captured_bits()
    }

    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }
}

/// Known properties associated to particular FTDI chip types.
#[derive(Debug)]
struct FtdiProperties {
    /// The size of the device's RX buffer.
    ///
    /// We can push down this many bytes to the device in one batch.
    buffer_size: usize,

    /// The maximum TCK clock speed supported by the device, in kHz.
    max_clock: u32,

    /// Whether the device supports the divide-by-5 clock mode for "FT2232D compatibility".
    ///
    /// Newer devices have 60MHz internal clocks, instead of 12MHz, however, they still
    /// fall back to 12MHz by default. This flag indicates whether we can disable the clock divider.
    has_divide_by_5: bool,
}

impl TryFrom<(FtdiDevice, Option<ChipType>)> for FtdiProperties {
    type Error = FtdiError;

    fn try_from((ftdi, chip_type): (FtdiDevice, Option<ChipType>)) -> Result<Self, Self::Error> {
        let chip_type = match chip_type {
            Some(ty) => ty,
            None => {
                tracing::warn!("Unknown FTDI chip. Assuming {:?}", ftdi.fallback_chip_type);
                ftdi.fallback_chip_type
            }
        };

        let properties = match chip_type {
            ChipType::FT2232H | ChipType::FT4232H => Self {
                buffer_size: 4096,
                max_clock: 30_000,
                has_divide_by_5: true,
            },
            ChipType::FT232H => Self {
                buffer_size: 1024,
                max_clock: 30_000,
                has_divide_by_5: true,
            },
            ChipType::FT2232C => Self {
                buffer_size: 128,
                max_clock: 6_000,
                has_divide_by_5: false,
            },
            not_mpsse => {
                tracing::warn!("Unsupported FTDI chip: {:?}", not_mpsse);
                return Err(FtdiError::UnsupportedChipType(not_mpsse));
            }
        };

        Ok(properties)
    }
}

#[derive(Debug, Clone, Copy)]
struct FtdiDevice {
    /// The (VID, PID) pair of this device.
    id: (u16, u16),

    /// FTDI chip type to use if the device is not recognized.
    ///
    /// "FTDI compatible" devices may use the same VID/PID pair as an FTDI device, but
    /// they may be implemented by a completely third party solution. In this case,
    /// we still try the same `bcdDevice` based detection, but if it fails, we fall back
    /// to this chip type.
    fallback_chip_type: ChipType,

    /// GPIO layout for this device.
    ///
    /// This defines the initial pin state and available signals.
    /// For devices that need product string matching (like Digilent), this is
    /// the default layout; product string matching happens in `JtagAdapter::attach()`.
    layout: &'static ProbeLayout,
}

impl FtdiDevice {
    fn matches(&self, device: &DeviceInfo) -> bool {
        self.id == (device.vendor_id(), device.product_id())
    }
}

/// Known FTDI device variants.
static FTDI_COMPAT_DEVICES: &[FtdiDevice] = &[
    //
    // --- FTDI VID/PID pairs ---
    //
    // FTDI Ltd. FT2232C/D/H Dual UART/FIFO IC
    // Used by Digilent HS1 and other generic FTDI adapters.
    // Product string matching is used in attach() to select Digilent layouts.
    FtdiDevice {
        id: (0x0403, 0x6010),
        fallback_chip_type: ChipType::FT2232C,
        layout: &gpio::GENERIC_FTDI,
    },
    // FTDI Ltd. FT4232H Quad HS USB-UART/FIFO IC
    FtdiDevice {
        id: (0x0403, 0x6011),
        fallback_chip_type: ChipType::FT4232H,
        layout: &gpio::GENERIC_FTDI,
    },
    // FTDI Ltd. FT232H Single HS USB-UART/FIFO IC
    // Used by Digilent HS2/HS3 and other generic FTDI adapters.
    // Product string matching is used in attach() to select Digilent layouts.
    FtdiDevice {
        id: (0x0403, 0x6014),
        fallback_chip_type: ChipType::FT232H,
        layout: &gpio::GENERIC_FTDI,
    },
    // Olimex Ltd. ARM-USB-OCD
    FtdiDevice {
        id: (0x15ba, 0x0003),
        fallback_chip_type: ChipType::FT2232C,
        layout: &gpio::OLIMEX_ARM_USB_OCD,
    },
    // Olimex Ltd. ARM-USB-TINY
    FtdiDevice {
        id: (0x15ba, 0x0004),
        fallback_chip_type: ChipType::FT2232C,
        layout: &gpio::OLIMEX_ARM_USB_TINY,
    },
    // Olimex Ltd. ARM-USB-TINY-H
    FtdiDevice {
        id: (0x15ba, 0x002a),
        fallback_chip_type: ChipType::FT2232H,
        layout: &gpio::OLIMEX_ARM_USB_TINY_H,
    },
    // Olimex Ltd. ARM-USB-OCD-H
    FtdiDevice {
        id: (0x15ba, 0x002b),
        fallback_chip_type: ChipType::FT2232H,
        layout: &gpio::OLIMEX_ARM_USB_OCD_H,
    },
];

fn get_device_info(device: &DeviceInfo) -> Option<DebugProbeInfo> {
    FTDI_COMPAT_DEVICES.iter().find_map(|ftdi| {
        ftdi.matches(device).then(|| DebugProbeInfo {
            identifier: device.product_string().unwrap_or("FTDI").to_string(),
            vendor_id: device.vendor_id(),
            product_id: device.product_id(),
            serial_number: device.serial_number().map(|s| s.to_string()),
            probe_factory: &FtdiProbeFactory,
            hid_interface: None,
        })
    })
}

#[tracing::instrument(skip_all)]
fn list_ftdi_devices() -> Vec<DebugProbeInfo> {
    match nusb::list_devices().wait() {
        Ok(devices) => devices
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(e) => {
            tracing::warn!("error listing FTDI devices: {e}");
            vec![]
        }
    }
}
