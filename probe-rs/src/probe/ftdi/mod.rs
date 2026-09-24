//! FTDI-based debug probes.
use crate::{
    architecture::{
        arm::{
            ArmCommunicationInterface, ArmDebugInterface, ArmError, sequences::ArmDebugSequence,
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
        BitbangSwd, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
        IoSequenceItem, JtagChain, JtagChainAccess, JtagChainState, JtagOp, JtagProbe,
        ProbeCreationError, ProbeFactory, SwdProbe, SwdSettings, WireProtocol,
        jtag::{TapState, distribute_captures, enter_tdi, exchange_leaves_shift},
        list::{ProbeListItem, usb_probe_accessibility},
        queue::{BatchExecutionError, Results},
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

use crate::probe::Batch;

use command_compacter::Command;
use ftdaye::{ChipType, error::FtdiError};

#[derive(Debug)]
struct JtagAdapter {
    device: ftdaye::Device,
    speed_khz: u32,

    commands: Vec<u8>,

    /// For each command that captures bits, stores how many bits are captured.
    in_bit_counts: Vec<usize>,
    in_bits: BitVec,
    ftdi: FtdiProperties,
}

impl JtagAdapter {
    /// Map a USB interface number from `--probe VID:PID-INTERFACE` to an FTDI channel.
    ///
    /// Multi-channel FTDI chips (FT2232H, FT4232H) expose each channel as a
    /// separate USB interface. The mapping is: 0 = Channel A, 1 = Channel B,
    /// 2 = Channel C, 3 = Channel D. Defaults to Channel A when no interface
    /// is specified.
    fn map_interface(usb_interface: Option<u8>) -> ftdaye::Interface {
        match usb_interface {
            Some(0) | None => ftdaye::Interface::A,
            Some(1) => ftdaye::Interface::B,
            Some(2) => ftdaye::Interface::C,
            Some(3) => ftdaye::Interface::D,
            Some(n) => {
                tracing::warn!(
                    "FTDI interface number {n} is out of range (0..=3), defaulting to Channel A"
                );
                ftdaye::Interface::A
            }
        }
    }

    fn open(
        ftdi: FtdiDevice,
        usb_device: DeviceInfo,
        usb_interface: Option<u8>,
    ) -> Result<Self, DebugProbeError> {
        let interface = Self::map_interface(usb_interface);
        let device = ftdaye::Builder::new()
            .with_interface(interface)
            .with_read_timeout(Duration::from_secs(5))
            .with_write_timeout(Duration::from_secs(5))
            .usb_open(usb_device)?;

        let ftdi = FtdiProperties::try_from((ftdi, device.chip_type()))?;

        Ok(Self {
            device,
            speed_khz: 1000,
            commands: vec![],
            in_bit_counts: vec![],
            in_bits: BitVec::new(),
            ftdi,
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

        let (output, direction) = self.pin_layout();
        self.device.set_pins(output, direction)?;

        self.apply_clock_speed(self.speed_khz)?;

        self.device.disable_loopback()?;

        Ok(())
    }

    fn pin_layout(&self) -> (u16, u16) {
        let (output, direction) = match (
            self.device.vendor_id(),
            self.device.product_id(),
            self.device.product_string().unwrap_or(""),
        ) {
            // Digilent HS3
            (0x0403, 0x6014, "Digilent USB Device") => (0x2088, 0x308b),
            // Digilent HS2
            (0x0403, 0x6014, "Digilent Adept USB Device") => (0x00e8, 0x60eb),
            // Digilent HS1
            (0x0403, 0x6010, "Digilent Adept USB Device") => (0x0088, 0x008b),
            // Built-in Digilent HS1 (on-board)
            (0x0403, 0x6010, "Digilent USB Device") => (0x0088, 0x008b),
            // Other devices:
            // TMS starts high
            // TMS, TDO and TCK are outputs
            _ => (0x0008, 0x000b),
        };
        (output, direction)
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
        let timeout = Duration::from_millis(100);

        let expected_bits = std::mem::take(&mut self.in_bit_counts);
        let reply_slots = expected_bits.len();

        // Read the exact number of bytes that the commands make. A read of more bytes gets only a
        // status message from the device, and the device sends a status message only one time in
        // each period of the latency timer. Such a read is therefore very slow.
        let mut reply = vec![0; reply_slots];
        let mut received = 0;
        while received < reply_slots {
            let read = self
                .device
                .read(&mut reply[received..])
                .map_err(FtdiError::from)?;

            received += read;

            if read > 0 {
                t0 = Instant::now();
            }

            if t0.elapsed() > timeout {
                tracing::warn!("Read {} bytes, expected {}", received, reply_slots);
                return Err(DebugProbeError::Timeout);
            }
        }

        for (byte, count) in reply.into_iter().zip(expected_bits) {
            let bits = byte >> (8 - count);
            self.in_bits
                .extend_from_bitslice(&bits.view_bits::<Lsb0>()[..count]);
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
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

    fn append_commands(&mut self, commands: &[Command]) -> Result<(), DebugProbeError> {
        for command in commands {
            self.append_command(command.clone())?;
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

    fn run_jtag_batch(
        &mut self,
        start: TapState,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> Result<(TapState, Results), BatchExecutionError<DebugProbeError>> {
        let (state, commands) = collect_ftdi_commands(start, batch)?;
        if let Err(error) = self.append_commands(&commands) {
            return Err(BatchExecutionError::new_from_debug_probe(
                error,
                Results::new(),
            ));
        }

        if let Err(error) = self.flush() {
            return Err(BatchExecutionError::new_from_debug_probe(
                error,
                Results::new(),
            ));
        }

        let captured = match self.read_captured_bits() {
            Ok(bits) => bits,
            Err(error) => {
                return Err(BatchExecutionError::new_from_debug_probe(
                    error,
                    Results::new(),
                ));
            }
        };

        let ops: Vec<_> = batch.iter().collect();
        let results = distribute_captures(ops, &captured, Results::new())?;

        Ok((state, results))
    }
}

fn collect_ftdi_commands(
    start: TapState,
    batch: &Batch<JtagOp, DebugProbeError>,
) -> Result<(TapState, Vec<Command>), BatchExecutionError<DebugProbeError>> {
    let ops: Vec<_> = batch.iter().collect();
    let mut state = start;
    let mut commands = Vec::new();
    let mut skip_enter_path_bits = 0usize;
    let results = Results::new();

    for (index, (id, op)) in ops.iter().enumerate() {
        match op {
            JtagOp::EnterState(target) => {
                let target = *target;
                let path = &state.path_to(target)[skip_enter_path_bits..];
                skip_enter_path_bits = 0;
                commands.extend(Command::encode_tms_path(path, enter_tdi(target)));
                state = target;
            }
            JtagOp::Exchange { data, capture } => {
                if state != TapState::ShiftIr && state != TapState::ShiftDr {
                    return Err(BatchExecutionError::new_from_debug_probe(
                        DebugProbeError::Other(format!(
                            "Exchange in state {state:?}, but ShiftIr or ShiftDr is required"
                        )),
                        results,
                    ));
                }
                let merge_exit = exchange_leaves_shift(state, ops.get(index + 1).map(|(_, op)| op));
                let do_capture = *capture && id.should_capture();
                commands.extend(Command::encode_tdi_exchange(data, merge_exit, do_capture));
                if merge_exit {
                    skip_enter_path_bits = 1;
                }
            }
            JtagOp::ClockTck { count } => {
                commands.extend(Command::encode_clock_tck(*count));
            }
        }
    }

    Ok((state, commands))
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
            adapter: JtagAdapter::open(ftdi, probes.pop().unwrap(), selector.interface)?,
            jtag_state: JtagChainState::default(),
            swd_settings: SwdSettings::default(),
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        list_ftdi_devices()
    }

    fn list_probes_filtered(&self, selector: Option<&DebugProbeSelector>) -> Vec<ProbeListItem> {
        // FTDI probes are enumerated as one entry per USB device. The interface/channel
        // (A/B/C/D) is not stored in DebugProbeInfo; it is a runtime selection passed
        // through to open(). The default list_probes_filtered() filters by interface,
        // which causes "no probe found" when the user specifies e.g. `--probe VID:PID-1`
        // to select Channel B on an FT2232H, because interface is always None in the
        // listed entries.
        //
        // Match only on VID, PID, and (optionally) serial number, ignoring interface.
        self.list_probes()
            .into_iter()
            .filter(|probe| {
                selector.as_ref().is_none_or(|s| {
                    probe.info.vendor_id == s.vendor_id
                        && probe.info.product_id == s.product_id
                        && s.serial_number.as_ref().is_none_or(|sn| {
                            if let Some(probe_sn) = &probe.info.serial_number {
                                probe_sn == sn
                            } else {
                                sn.is_empty()
                            }
                        })
                })
            })
            .collect()
    }
}

/// An FTDI-based debug probe.
#[derive(Debug)]
pub struct FtdiProbe {
    adapter: JtagAdapter,
    jtag_state: JtagChainState,
    swd_settings: SwdSettings,
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
        Ok(())
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        // TODO we could add this by using a GPIO. However, different probes may connect
        // different pins (if any) to the reset line, so we would need to make this configurable.
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset",
        })
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_assert",
        })
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::NotImplemented {
            function_name: "target_reset_deassert",
        })
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

    fn try_as_jtag_chain(&mut self) -> Option<JtagChain<'_>> {
        Some(JtagChain::new(self))
    }

    fn try_as_swd_probe_mut(&mut self) -> Option<&mut dyn SwdProbe> {
        Some(self)
    }

    fn try_as_jtag_chain_access_mut(&mut self) -> Option<&mut dyn JtagChainAccess> {
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
        let settings = SwdProbe::swd_settings(self.as_ref());
        Ok(ArmCommunicationInterface::create_jtag(
            self, settings, sequence, true,
        ))
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

impl JtagChainAccess for FtdiProbe {
    fn chain_state(&mut self) -> &mut JtagChainState {
        &mut self.jtag_state
    }

    fn chain_state_ref(&self) -> &JtagChainState {
        &self.jtag_state
    }
}

impl JtagProbe for FtdiProbe {
    fn run_batch(
        &mut self,
        batch: &Batch<JtagOp, DebugProbeError>,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let start = self.jtag_state.tap_state;
        let (state, results) = self.adapter.run_jtag_batch(start, batch)?;
        self.jtag_state.tap_state = state;
        Ok(results)
    }
}

impl BitbangSwd for FtdiProbe {
    fn swd_io<S>(&mut self, _swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>,
    {
        Err(DebugProbeError::NotImplemented {
            function_name: "swd_io",
        })
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
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
    FtdiDevice {
        id: (0x0403, 0x6010),
        fallback_chip_type: ChipType::FT2232C,
    },
    // FTDI Ltd. FT4232H Quad HS USB-UART/FIFO IC
    FtdiDevice {
        id: (0x0403, 0x6011),
        fallback_chip_type: ChipType::FT4232H,
    },
    // FTDI Ltd. FT232H Single HS USB-UART/FIFO IC
    FtdiDevice {
        id: (0x0403, 0x6014),
        fallback_chip_type: ChipType::FT232H,
    },
    //
    // --- Third-party VID/PID pairs ---
    //
    // Olimex Ltd. ARM-USB-OCD
    FtdiDevice {
        id: (0x15ba, 0x0003),
        fallback_chip_type: ChipType::FT2232C,
    },
    // Olimex Ltd. ARM-USB-TINY
    FtdiDevice {
        id: (0x15ba, 0x0004),
        fallback_chip_type: ChipType::FT2232C,
    },
    // Olimex Ltd. ARM-USB-TINY-H
    FtdiDevice {
        id: (0x15ba, 0x002a),
        fallback_chip_type: ChipType::FT2232H,
    },
    // Olimex Ltd. ARM-USB-OCD-H
    FtdiDevice {
        id: (0x15ba, 0x002b),
        fallback_chip_type: ChipType::FT2232H,
    },
];

fn get_device_info(device: &DeviceInfo) -> Option<ProbeListItem> {
    FTDI_COMPAT_DEVICES.iter().find_map(|ftdi| {
        ftdi.matches(device).then(|| ProbeListItem {
            info: DebugProbeInfo {
                identifier: device.product_string().unwrap_or("FTDI").to_string(),
                vendor_id: device.vendor_id(),
                product_id: device.product_id(),
                serial_number: device.serial_number().map(|s| s.to_string()),
                probe_factory: &FtdiProbeFactory,
                is_hid_interface: false,
                interface: None,
            },
            accessibility: usb_probe_accessibility(device),
        })
    })
}

#[tracing::instrument(skip_all)]
fn list_ftdi_devices() -> Vec<ProbeListItem> {
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

#[cfg(test)]
mod golden_tests {
    use super::collect_ftdi_commands;
    use super::command_compacter::decoder::decode_commands_full;
    use crate::probe::jtag::golden::{
        REGISTER_WRITE_EIGHT_IDLE, SHIFT_DR_ONE_TAP_FORTY_ONE, SHIFT_DR_ONE_TAP_ONE,
        SHIFT_DR_ONE_TAP_SIXTY_FOUR, SHIFT_DR_ONE_TAP_THIRTY_TWO, SHIFT_DR_THREE_TAP_FORTY_ONE,
        SHIFT_DR_THREE_TAP_ONE, SHIFT_DR_THREE_TAP_SIXTY_FOUR, SHIFT_DR_THREE_TAP_THIRTY_TWO,
        SHIFT_IR_ONE_TAP, SHIFT_IR_THREE_TAP, assert_triples_eq, build_dr_exchange,
        build_ir_exchange, move_literal, one_tap_params, three_tap_params,
    };
    use crate::probe::jtag::{JtagBatch, TapState};

    const STABLE_STATES: [TapState; 6] = [
        TapState::TestLogicReset,
        TapState::RunTestIdle,
        TapState::ShiftIr,
        TapState::ShiftDr,
        TapState::PauseIr,
        TapState::PauseDr,
    ];

    fn triples_for_batch(start: TapState, batch: &JtagBatch) -> Vec<(bool, bool, bool)> {
        let (_, commands) = collect_ftdi_commands(start, batch).unwrap();
        let mut bytes = Vec::new();
        for command in &commands {
            command.encode(&mut bytes);
        }
        decode_commands_full(&bytes)
    }

    #[test]
    fn move_to_state_matches_golden() {
        for from in STABLE_STATES {
            for to in STABLE_STATES {
                if to == TapState::TestLogicReset {
                    continue;
                }
                let mut batch = JtagBatch::new();
                batch.enter(to);
                let triples = triples_for_batch(from, &batch);
                let (tms, tdi, cap) = move_literal(from, to);
                assert_triples_eq(&triples, tms, tdi, cap);
            }
        }
    }

    #[test]
    fn shift_ir_matches_golden() {
        let cases = [
            (SHIFT_IR_ONE_TAP, one_tap_params()),
            (SHIFT_IR_THREE_TAP, three_tap_params()),
        ];
        for (literal, params) in cases {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftIr);
            batch.exchange_no_capture(build_ir_exchange(params, 0b10110, 5));
            batch.enter(TapState::RunTestIdle);
            let triples = triples_for_batch(TapState::TestLogicReset, &batch);
            assert_triples_eq(&triples, literal.0, literal.1, literal.2);
        }
    }

    #[test]
    fn shift_dr_matches_golden() {
        let cases = [
            (SHIFT_DR_ONE_TAP_ONE, one_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_ONE_TAP_THIRTY_TWO,
                one_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_ONE_TAP_FORTY_ONE,
                one_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_ONE_TAP_SIXTY_FOUR,
                one_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
            (SHIFT_DR_THREE_TAP_ONE, three_tap_params(), &[0x01u8][..], 1),
            (
                SHIFT_DR_THREE_TAP_THIRTY_TWO,
                three_tap_params(),
                &[0x78, 0x56, 0x34, 0x12],
                32,
            ),
            (
                SHIFT_DR_THREE_TAP_FORTY_ONE,
                three_tap_params(),
                &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06],
                41,
            ),
            (
                SHIFT_DR_THREE_TAP_SIXTY_FOUR,
                three_tap_params(),
                &[0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11],
                64,
            ),
        ];
        for (literal, params, bytes, len) in cases {
            let mut batch = JtagBatch::new();
            batch.enter(TapState::ShiftDr);
            batch.exchange_no_capture(build_dr_exchange(params, bytes, len));
            batch.enter(TapState::RunTestIdle);
            let triples = triples_for_batch(TapState::TestLogicReset, &batch);
            assert_triples_eq(&triples, literal.0, literal.1, literal.2);
        }
    }

    #[test]
    fn register_write_with_idle_matches_golden() {
        let bytes = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
        let params = one_tap_params();
        let mut batch = JtagBatch::new();
        batch.enter(TapState::ShiftIr);
        batch.exchange_no_capture(build_ir_exchange(params, 0b10110, 5));
        batch.enter(TapState::ShiftDr);
        batch.exchange_no_capture(build_dr_exchange(params, &bytes, 41));
        batch.enter(TapState::RunTestIdle);
        batch.clock(8);
        let triples = triples_for_batch(TapState::TestLogicReset, &batch);
        assert_triples_eq(
            &triples,
            REGISTER_WRITE_EIGHT_IDLE.0,
            REGISTER_WRITE_EIGHT_IDLE.1,
            REGISTER_WRITE_EIGHT_IDLE.2,
        );
    }
}
