use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::xtensa::communication_interface::XtensaCommunicationInterface;
use crate::architecture::{
    arm::communication_interface::UninitializedArmProbe,
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::probe::common::{JtagDriverState, RawJtagIo};
use crate::{
    probe::{DebugProbe, JTAGAccess, ProbeCreationError, ProbeDriver, ScanChainElement},
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, WireProtocol,
};
use anyhow::anyhow;
use bitvec::prelude::*;
use nusb::DeviceInfo;
use std::io::{Read, Write};
use std::time::Duration;

mod ftdi_impl;
use ftdi_impl as ftdi;

mod command_compacter;

use command_compacter::Command;

impl From<ftdi::Error> for DebugProbeError {
    fn from(e: ftdi::Error) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(e))
    }
}

#[derive(Debug)]
struct JtagAdapter {
    device: ftdi::Device,
    ftdi: FtdiDevice,
    speed_khz: u32,

    buffer_size: usize,

    command: Command,
    commands: Vec<u8>,
    in_bit_counts: Vec<usize>,
    in_bits: BitVec<u8, Lsb0>,
}

impl JtagAdapter {
    fn open(ftdi: &FtdiDevice) -> Result<Self, ftdi::Error> {
        let mut builder = ftdi::Builder::new();
        builder.set_interface(ftdi::Interface::A)?;
        let device = builder.usb_open(ftdi.id.0, ftdi.id.1)?;

        Ok(Self {
            device,
            ftdi: ftdi.clone(),
            speed_khz: 1000,
            buffer_size: ftdi.buffer_size,
            command: Command::default(),
            commands: vec![],
            in_bit_counts: vec![],
            in_bits: BitVec::new(),
        })
    }

    pub fn attach(&mut self) -> Result<(), ftdi::Error> {
        self.device.usb_reset()?;
        self.device.set_latency_timer(1)?;
        self.device.set_bitmode(0x0b, ftdi::BitMode::Mpsse)?;
        self.device.usb_purge_buffers()?;

        let mut junk = vec![];
        let _ = self.device.read_to_end(&mut junk);

        // Minimal values, may not work with all probes
        let output: u16 = 0x0008;
        let direction: u16 = 0x000b;
        self.device
            .write_all(&[0x80, output as u8, direction as u8])?;
        self.device
            .write_all(&[0x82, (output >> 8) as u8, (direction >> 8) as u8])?;

        self.apply_clock_speed(self.speed_khz)?;

        // Disable loopback
        self.device.write_all(&[0x85])?;

        Ok(())
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed_khz(&mut self, speed_khz: u32) -> u32 {
        self.speed_khz = speed_khz;
        self.speed_khz
    }

    fn apply_clock_speed(&mut self, speed_khz: u32) -> Result<u32, ftdi::Error> {
        // Disable divide-by-5 mode if available
        if self.ftdi.has_divide_by_5 {
            self.device.write_all(&[0x8A])?;
        } else {
            // Force enable divide-by-5 mode if not available or unknown
            self.device.write_all(&[0x8B])?;
        }

        // If `speed_khz` is not a divisor of the maximum supported speed, we need to round up
        let is_exact = self.ftdi.max_clock_frequency_khz % speed_khz == 0;

        // If `speed_khz` is 0, use the maximum supported speed
        let divisor = (self
            .ftdi
            .max_clock_frequency_khz
            .checked_div(speed_khz)
            .unwrap_or(1)
            - is_exact as u32)
            .min(0xFFFF);

        let actual_speed = self.ftdi.max_clock_frequency_khz / (divisor + 1);

        tracing::info!(
            "Setting speed to {} kHz (divisor: {}, actual speed: {} kHz)",
            speed_khz,
            divisor,
            actual_speed
        );

        let [l, h] = (divisor as u16).to_le_bytes();
        self.device.write_all(&[0x86, l, h])?;

        self.speed_khz = actual_speed;
        Ok(actual_speed)
    }

    fn read_response(&mut self) -> Result<(), DebugProbeError> {
        if self.in_bit_counts.is_empty() {
            return Ok(());
        }

        let mut t0 = std::time::Instant::now();
        let timeout = Duration::from_millis(10);

        let mut reply = Vec::with_capacity(self.in_bit_counts.len());
        while reply.len() < self.in_bit_counts.len() {
            if t0.elapsed() > timeout {
                tracing::warn!(
                    "Read {} bytes, expected {}",
                    reply.len(),
                    self.in_bit_counts.len()
                );
                return Err(DebugProbeError::Timeout);
            }

            let read = self
                .device
                .read_to_end(&mut reply)
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

            if read > 0 {
                t0 = std::time::Instant::now();
            }
        }

        if reply.len() != self.in_bit_counts.len() {
            return Err(DebugProbeError::Other(anyhow!(
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
        self.read_response()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        Ok(())
    }

    fn append_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        tracing::debug!("Appending {:?}", command);
        // 1 byte is reserved for the send immediate command
        if self.commands.len() + command.len() + 1 >= self.buffer_size {
            self.send_buffer()?;
            self.read_response()
                .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;
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
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        self.commands.clear();

        Ok(())
    }

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.flush()?;

        Ok(std::mem::take(&mut self.in_bits))
    }
}

pub struct FtdiProbeSource;

impl std::fmt::Debug for FtdiProbeSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FTDI").finish()
    }
}

impl ProbeDriver for FtdiProbeSource {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        // Only open FTDI-compatible probes

        let device = match nusb::list_devices() {
            Ok(devices) => {
                let mut matched = None;
                for device in devices.filter(|info| selector.matches(info)) {
                    // FTDI devices don't have serial numbers, so we can only match on VID/PID.
                    // Bail if we find more than one matching device.
                    if matched.is_some() {
                        return Err(DebugProbeError::ProbeCouldNotBeCreated(
                            ProbeCreationError::Other("Multiple FTDI devices found. Please unplug all but one FTDI device."),
                        ));
                    }

                    matched = FTDI_COMPAT_DEVICES
                        .iter()
                        .find(|ftdi| ftdi.matches(&device));
                }

                matched
            }
            Err(_) => None,
        };

        let Some(device) = device else {
            return Err(DebugProbeError::ProbeCouldNotBeCreated(
                ProbeCreationError::NotFound,
            ));
        };

        let adapter =
            JtagAdapter::open(device).map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let probe = FtdiProbe {
            adapter,
            jtag_state: JtagDriverState::default(),
        };
        tracing::debug!("opened probe: {:?}", probe);
        Ok(Box::new(probe))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        list_ftdi_devices()
    }
}

#[derive(Debug)]
pub struct FtdiProbe {
    adapter: JtagAdapter,
    jtag_state: JtagDriverState,
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

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        tracing::info!("Setting scan chain to {:?}", scan_chain);
        self.jtag_state.expected_scan_chain = Some(scan_chain);
        Ok(())
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching...");

        self.adapter
            .attach()
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        let chain = self.scan_chain()?;
        tracing::info!("Found {} TAPs on reset scan", chain.len());

        if chain.len() > 1 {
            tracing::warn!("More than one TAP detected, defaulting to tap0");
        }

        self.select_target(&chain, 0)
    }

    fn detach(&mut self) -> Result<(), crate::Error> {
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset");
        unimplemented!()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset_assert");
        unimplemented!()
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::error!("FTDI target_reset_deassert");
        unimplemented!()
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

    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        match RiscvCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn has_riscv_interface(&self) -> bool {
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        todo!()
    }

    fn try_get_xtensa_interface(
        self: Box<Self>,
    ) -> Result<XtensaCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        // This probe is intended for Xtensa.
        match XtensaCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn has_xtensa_interface(&self) -> bool {
        true
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

    fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.adapter.read_captured_bits()
    }

    fn state_mut(&mut self) -> &mut JtagDriverState {
        &mut self.jtag_state
    }

    fn state(&self) -> &JtagDriverState {
        &self.jtag_state
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.adapter.flush()
    }
}

#[derive(Debug, Clone)]
struct FtdiDevice {
    /// The (VID, PID) pair of this device.
    id: (u16, u16),

    /// If set, only an exact match of this product string will be accepted.
    product_string: Option<&'static str>,

    /// The size of the device's TX/RX buffers.
    buffer_size: usize,

    /// The maximum output clock frequency of the device in kHz.
    max_clock_frequency_khz: u32,

    /// Whether the device supports a divide-by-5 mode for FT2232D compatibility.
    has_divide_by_5: bool,
}

const BUFFER_SIZE_FTDI2232C_D: usize = 128;
const BUFFER_SIZE_FTDI232H: usize = 1024;
const BUFFER_SIZE_FTDI2232H: usize = 4096;

impl FtdiDevice {
    const fn ft2232cd(id: (u16, u16), product_string: Option<&'static str>) -> Self {
        Self {
            id,
            product_string,
            buffer_size: BUFFER_SIZE_FTDI2232C_D,
            max_clock_frequency_khz: 6_000,
            has_divide_by_5: false,
        }
    }

    const fn ft232h(id: (u16, u16), product_string: Option<&'static str>) -> Self {
        Self {
            id,
            product_string,
            buffer_size: BUFFER_SIZE_FTDI232H,
            max_clock_frequency_khz: 30_000,
            has_divide_by_5: true,
        }
    }

    const fn ft2232h(id: (u16, u16), product_string: Option<&'static str>) -> Self {
        Self {
            id,
            product_string,
            buffer_size: BUFFER_SIZE_FTDI2232H,
            max_clock_frequency_khz: 30_000,
            has_divide_by_5: true,
        }
    }

    const fn ft4232h(id: (u16, u16), product_string: Option<&'static str>) -> FtdiDevice {
        Self {
            id,
            product_string,
            buffer_size: BUFFER_SIZE_FTDI2232H,
            max_clock_frequency_khz: 30_000,
            has_divide_by_5: true,
        }
    }

    fn matches(&self, device: &DeviceInfo) -> bool {
        self.id == (device.vendor_id(), device.product_id())
            && (self.product_string.is_none() || self.product_string == device.product_string())
    }
}

/// Known FTDI device variants. Matched from first to last, meaning that more specific devices
/// (i.e. those wih product strings) should be listed first.
static FTDI_COMPAT_DEVICES: &[FtdiDevice] = &[
    // FTDI Ltd. FT2232H Dual UART/FIFO IC
    FtdiDevice::ft2232h((0x0403, 0x6010), Some("Dual RS232-HS")),
    // Unidentified FTDI Ltd. FT2232C/D/H Dual UART/FIFO IC -> fall back to FT2232D
    FtdiDevice::ft2232cd((0x0403, 0x6010), None),
    // FTDI Ltd. FT4232H Quad HS USB-UART/FIFO IC
    FtdiDevice::ft4232h((0x0403, 0x6011), None),
    // FTDI Ltd. FT232H Single HS USB-UART/FIFO IC
    FtdiDevice::ft232h((0x0403, 0x6014), None),
    // Olimex Ltd. ARM-USB-OCD JTAG interface, FTDI2232C
    FtdiDevice::ft2232cd((0x15ba, 0x0003), None),
    // Olimex Ltd. ARM-USB-TINY JTAG interface, FTDI2232C
    FtdiDevice::ft2232cd((0x15ba, 0x0004), None),
    // Olimex Ltd. ARM-USB-TINY-H JTAG interface, FTDI2232H
    FtdiDevice::ft2232h((0x15ba, 0x002a), None),
    // Olimex Ltd. ARM-USB-OCD-H JTAG interface, FTDI2232H
    FtdiDevice::ft2232h((0x15ba, 0x002b), None),
];

fn get_device_info(device: &DeviceInfo) -> Option<DebugProbeInfo> {
    if !FTDI_COMPAT_DEVICES.iter().any(|ftdi| ftdi.matches(device)) {
        return None;
    }

    Some(DebugProbeInfo {
        identifier: device.product_string().unwrap_or("FTDI").to_string(),
        vendor_id: device.vendor_id(),
        product_id: device.product_id(),
        serial_number: device.serial_number().map(|s| s.to_string()),
        probe_type: &FtdiProbeSource,
        hid_interface: None,
    })
}

#[tracing::instrument(skip_all)]
fn list_ftdi_devices() -> Vec<DebugProbeInfo> {
    match nusb::list_devices() {
        Ok(devices) => devices
            .filter_map(|device| get_device_info(&device))
            .collect(),
        Err(_) => vec![],
    }
}
