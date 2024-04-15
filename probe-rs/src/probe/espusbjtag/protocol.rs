use bitvec::prelude::*;
use nusb::{
    transfer::{Direction, EndpointType},
    DeviceInfo,
};
use std::{
    fmt::Debug,
    time::{Duration, Instant},
};

use crate::probe::{
    espusbjtag::EspUsbJtagFactory, usb_util::InterfaceExt, DebugProbeError, DebugProbeInfo,
    DebugProbeSelector, ProbeCreationError,
};

const JTAG_PROTOCOL_CAPABILITIES_VERSION: u8 = 1;
const JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE: u8 = 1;
// The internal repeat counter register is 10 bits. We don't count the initial execution,
// so the maximum repeat counter value is 1023.
const MAX_COMMAND_REPETITIONS: usize = 1023;
// Each command is 4 bits, i.e. 2 commands per byte:
const OUT_BUFFER_SIZE: usize = OUT_EP_BUFFER_SIZE * 2;
const OUT_EP_BUFFER_SIZE: usize = 64;
const IN_EP_BUFFER_SIZE: usize = 64;
const HW_FIFO_SIZE: usize = 4;
const USB_TIMEOUT: Duration = Duration::from_millis(5000);
const USB_DEVICE_CLASS: u8 = 0xFF;
const USB_DEVICE_SUBCLASS: u8 = 0xFF;
const USB_DEVICE_PROTOCOL: u8 = 0x01;
const USB_DEVICE_TRANSFER_TYPE: EndpointType = EndpointType::Bulk;

const USB_VID: u16 = 0x303A;
const USB_PID: u16 = 0x1001;

const DESCRIPTOR_JTAG_CAPABILITIES_TYPE: u8 = 0x20;
const DESCRIPTOR_JTAG_CAPABILITIES_INDEX: u8 = 0x00;

pub(super) struct ProtocolHandler {
    // The USB device handle.
    device_handle: nusb::Interface,

    /// The command in the queue and their additional repetitions.
    /// For now we do one command at a time.
    command_queue: Option<(RepeatableCommand, usize)>,
    /// The buffer for all commands to be sent to the target. This already contains `repeated`
    /// commands which is the interface's RLE mechanism to reduce the amount of data sent.
    output_buffer: Vec<Command>,
    /// A store for all the read bits (from the target) such that the BitIter the methods return
    /// can borrow and iterate over it.
    response: BitVec<u8, Lsb0>,
    pending_in_bits: usize,

    ep_out: u8,
    ep_in: u8,

    pub(crate) base_speed_khz: u32,
    pub(crate) div_min: u16,
    pub(crate) div_max: u16,
}

impl Debug for ProtocolHandler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtocolHandler")
            .field("command_queue", &self.command_queue)
            .field("output_buffer", &self.output_buffer)
            .field("response", &self.response)
            .field("ep_out", &self.ep_out)
            .field("ep_in", &self.ep_in)
            .field("base_speed_khz", &self.base_speed_khz)
            .field("div_min", &self.div_min)
            .field("div_max", &self.div_max)
            .finish()
    }
}

impl ProtocolHandler {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let device = nusb::list_devices()
            .map_err(ProbeCreationError::Usb)?
            .filter(is_espjtag_device)
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;

        let device_handle = device.open().map_err(ProbeCreationError::Usb)?;

        tracing::debug!("Aquired handle for probe");

        let config = device_handle.configurations().next().unwrap();

        tracing::debug!("Active config descriptor: {:?}", &config);

        let mut found = None;

        for interface in config.interfaces() {
            tracing::trace!("Interface {}", interface.interface_number());

            let mut ep_out = None;
            let mut ep_in = None;

            let descriptor = interface.alt_settings().next();
            if let Some(descriptor) = descriptor {
                if descriptor.class() == USB_DEVICE_CLASS
                    && descriptor.subclass() == USB_DEVICE_SUBCLASS
                    && descriptor.protocol() == USB_DEVICE_PROTOCOL
                {
                    for endpoint in descriptor.endpoints() {
                        tracing::trace!("Endpoint {}", endpoint.address());
                        if endpoint.transfer_type() == USB_DEVICE_TRANSFER_TYPE {
                            if endpoint.direction() == Direction::In {
                                ep_in = Some(endpoint.address());
                            } else {
                                ep_out = Some(endpoint.address());
                            }
                        }
                    }
                }
            }

            if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                tracing::debug!(
                    "Claiming interface {} with IN EP {} and OUT EP {}.",
                    interface.interface_number(),
                    ep_in,
                    ep_out
                );

                let iface = device_handle
                    .claim_interface(interface.interface_number())
                    .map_err(ProbeCreationError::Usb)?;

                found = Some((iface, ep_in, ep_out));
                break;
            }
        }

        let Some((iface, ep_in, ep_out)) = found else {
            return Err(ProbeCreationError::ProbeSpecific(
                "USB interface or endpoints could not be found.".into(),
            ));
        };

        let start = std::time::Instant::now();
        let buffer = loop {
            let buffer = device_handle
                .get_descriptor(
                    DESCRIPTOR_JTAG_CAPABILITIES_TYPE,
                    DESCRIPTOR_JTAG_CAPABILITIES_INDEX,
                    0,
                    USB_TIMEOUT,
                )
                .map_err(ProbeCreationError::Usb)?;
            if !buffer.is_empty() {
                break buffer;
            }
            if Instant::now() - start > USB_TIMEOUT {
                return Err(ProbeCreationError::Other(
                    "Timeout accessing device descriptor",
                ));
            }
        };

        let mut base_speed_khz = 1000;
        let mut div_min = 1;
        let mut div_max = 1;

        let protocol_version = buffer[0];
        tracing::debug!("{:02x?}", &buffer);
        tracing::debug!("Protocol version: {}", protocol_version);
        if protocol_version != JTAG_PROTOCOL_CAPABILITIES_VERSION {
            return Err(ProbeCreationError::ProbeSpecific(
                "Unknown capabilities descriptor version.".into(),
            ));
        }

        let length = buffer[1] as usize;

        let mut p = 2usize;
        while p < length {
            let typ = buffer[p];
            let length = buffer[p + 1];

            if typ == JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE {
                base_speed_khz =
                    ((((buffer[p + 3] as u16) << 8) | buffer[p + 2] as u16) as u64 * 10 / 2) as u32;
                div_min = ((buffer[p + 5] as u16) << 8) | buffer[p + 4] as u16;
                div_max = ((buffer[p + 7] as u16) << 8) | buffer[p + 6] as u16;
                tracing::info!("Found ESP USB JTAG adapter, base speed is {base_speed_khz}kHz. Available dividers: ({div_min}..{div_max})");
            } else {
                tracing::warn!("Unknown capabilities type {:01X?}", typ);
            }

            p += length as usize;
        }

        tracing::debug!("Succesfully attached to ESP USB JTAG.");

        Ok(Self {
            device_handle: iface,
            command_queue: None,
            output_buffer: Vec::with_capacity(OUT_BUFFER_SIZE),
            response: BitVec::new(),
            ep_out,
            ep_in,
            pending_in_bits: 0,

            base_speed_khz,
            div_min,
            div_max,
        })
    }

    /// Put a bit on TDI and possibly read one from TDO.
    /// to receive the bytes from this operations call [`ProtocolHandler::flush`]
    ///
    /// Note that if the internal buffer is exceeded bytes will be automatically flushed to usb device
    pub fn shift_bit(&mut self, tms: bool, tdi: bool, cap: bool) -> Result<(), DebugProbeError> {
        if cap && self.pending_in_bits == 128 * 8 {
            // From the ESP32-S3 TRM:
            // [A] command stream can cause at most 128 bytes of capture data to be
            // generated [...] without the host acting to receive the generated data. If
            // more data is generated anyway, the command stream is paused and the device
            // will not accept more commands before the generated capture data is read out.

            // Let's break the command stream here and flush the data.
            // We do this before we would capture the 1025th bit, so we don't do an
            // extra flush if we only ever want to capture 1024 bits.
            self.finalize_previous_command()?;
            self.send_buffer()?;
            self.receive_buffer()?;
        }

        self.push_command(RepeatableCommand::Clock { cap, tdi, tms })?;
        if cap {
            self.pending_in_bits += 1;
        }

        Ok(())
    }

    /// Sets the system reset signal on the target.
    pub fn set_reset(&mut self, srst: bool) -> Result<(), DebugProbeError> {
        self.finalize_previous_command()?;
        self.add_raw_command(Command::Reset(srst))?;
        self.flush()?;
        Ok(())
    }

    /// Adds a command to the command queue.
    /// This will properly add repeat commands if possible.
    fn push_command(&mut self, command: RepeatableCommand) -> Result<(), DebugProbeError> {
        if let Some((command_in_queue, ref mut repetitions)) = self.command_queue {
            if command == command_in_queue && *repetitions < MAX_COMMAND_REPETITIONS {
                *repetitions += 1;
                return Ok(());
            }

            let repetitions = *repetitions;
            self.write_stream(command_in_queue, repetitions)?;
        }

        self.command_queue = Some((command, 0));

        Ok(())
    }

    fn finalize_previous_command(&mut self) -> Result<(), DebugProbeError> {
        if let Some((command_in_queue, repetitions)) = self.command_queue.take() {
            self.write_stream(command_in_queue, repetitions)?;
        }

        Ok(())
    }

    /// Flushes pending commands and reads the captured bits from the target.
    ///
    /// The captured bits will be stored in the response buffer.
    pub(super) fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.finalize_previous_command()?;

        // Only flush if we have anything to do.
        if !self.output_buffer.is_empty() || self.pending_in_bits != 0 {
            tracing::debug!("Flushing ...");

            self.add_raw_command(Command::Flush)?;
            self.send_buffer()?;

            while self.pending_in_bits != 0 {
                self.receive_buffer()?;
            }
        }

        Ok(())
    }

    /// Flushes pending commands and reads the captured bits from the target.
    ///
    /// This method returns the response buffer and clears it. The returned buffer will contain
    /// all bits captured since the last call to `read_captured_bits`.
    pub(super) fn read_captured_bits(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.flush()?;

        Ok(std::mem::take(&mut self.response))
    }

    /// Writes a command one or multiple times into the raw buffer we send to the USB EP later
    /// or if the out buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn write_stream(
        &mut self,
        command: RepeatableCommand,
        repetitions: usize,
    ) -> Result<(), DebugProbeError> {
        tracing::trace!("add raw cmd {:?} reps={}", command, repetitions + 1);

        // Send the actual command.
        self.add_raw_command(command.into())?;
        self.add_repetitions(repetitions)?;

        Ok(())
    }

    /// Adds the required number of repetitions to the output buffer.
    fn add_repetitions(&mut self, mut repetitions: usize) -> Result<(), DebugProbeError> {
        // Send repetitions as many times as required.
        // We only send 2 bits with each repetition command as per the protocol.
        //
        // Non-repeat commands reset the `cmd_rep_count` to 0. Each subsequent repeat command
        // increases `cmd_rep_count`. The number of repetitions for each `Command::Repeat` are
        // calculated as `repeat_count x 4^cmd_rep_count`. This sounds complicated but essentially
        // we just have to shift in the repetition counter 2 bits at a time.
        while repetitions > 0 {
            self.add_raw_command(Command::Repeat((repetitions & 3) as u8))?;
            repetitions >>= 2;
        }

        Ok(())
    }

    /// Adds a single command to the output buffer and writes it to the USB EP if the buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn add_raw_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        self.output_buffer.push(command);

        // If we reach a maximal size of the output buffer, we flush.
        if self.output_buffer.len() == OUT_BUFFER_SIZE {
            self.send_buffer()?;
        }

        Ok(())
    }

    /// Sends the commands stored in the output buffer to the USB EP.
    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        let mut commands = [0; OUT_EP_BUFFER_SIZE];
        for (out, byte) in commands
            .iter_mut()
            .zip(self.output_buffer.chunks(2).map(|chunk| {
                let unibble: u8 = chunk[0].into();
                // Make sure we add an additional nibble to the command buffer if the number of
                // nibbles is odd, as we cannot send a standalone nibble.
                let lnibble: u8 = chunk.get(1).copied().unwrap_or(Command::Flush).into();

                (unibble << 4) | lnibble
            }))
        {
            *out = byte;
        }

        let len = (self.output_buffer.len() + 1) / 2;

        tracing::trace!(
            "Writing {} bytes ({} nibbles) to usb endpoint",
            len,
            self.output_buffer.len()
        );

        let mut commands = &commands[..len];
        while !commands.is_empty() {
            let bytes = self
                .device_handle
                .write_bulk(self.ep_out, commands, USB_TIMEOUT)
                .map_err(DebugProbeError::Usb)?;

            commands = &commands[bytes..];
        }

        // We only clear the output buffer on a successful transmission of all bytes.
        self.output_buffer.clear();

        // If there's more than a bufferful of data queing up in the jtag adapters IN endpoint, empty all but one buffer.
        while self.pending_in_bits > (IN_EP_BUFFER_SIZE + HW_FIFO_SIZE) * 8 {
            self.receive_buffer()?;
        }

        Ok(())
    }

    /// Tries to receive pending in bits from the USB EP.
    fn receive_buffer(&mut self) -> Result<(), DebugProbeError> {
        let count = ((self.pending_in_bits + 7) / 8).min(IN_EP_BUFFER_SIZE);
        let mut incoming = [0; IN_EP_BUFFER_SIZE];

        tracing::trace!("Receiving buffer, pending bits: {}", self.pending_in_bits);

        if self.pending_in_bits == 0 {
            return Ok(());
        }

        let read_bytes = self
            .device_handle
            .read_bulk(self.ep_in, &mut incoming, USB_TIMEOUT)
            .map_err(|e| {
                tracing::warn!(
                    "Something went wrong in read_bulk {:?} when trying to read {}bytes - pending_in_bits: {}",
                    e,
                    count,
                    self.pending_in_bits,
                );
                DebugProbeError::Usb(e)
            })?;

        let bits_in_buffer = self.pending_in_bits.min(read_bytes * 8);
        let incoming = &incoming[..count];

        tracing::trace!("Read: {:?}, length = {}", incoming, bits_in_buffer);
        self.pending_in_bits -= bits_in_buffer;

        let bs: &BitSlice<_, Lsb0> = BitSlice::from_slice(incoming);
        self.response.extend_from_bitslice(&bs[..bits_in_buffer]);

        Ok(())
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum RepeatableCommand {
    Clock { cap: bool, tdi: bool, tms: bool },
}

impl From<RepeatableCommand> for Command {
    fn from(command: RepeatableCommand) -> Self {
        match command {
            RepeatableCommand::Clock { cap, tdi, tms } => Command::Clock { cap, tdi, tms },
        }
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum Command {
    Clock { cap: bool, tdi: bool, tms: bool },
    Reset(bool),
    Flush,
    Repeat(u8),
}

impl From<Command> for u8 {
    fn from(command: Command) -> Self {
        match command {
            Command::Clock { cap, tdi, tms } => {
                (if cap { 4 } else { 0 } | if tms { 2 } else { 0 } | u8::from(tdi))
            }
            Command::Reset(srst) => 8 | u8::from(srst),
            Command::Flush => 0xA,
            Command::Repeat(repetitions) => 0xC + repetitions,
        }
    }
}

pub(super) fn is_espjtag_device(device: &DeviceInfo) -> bool {
    // Check the VID/PID.
    device.vendor_id() == USB_VID && device.product_id() == USB_PID
}

#[tracing::instrument(skip_all)]
pub(super) fn list_espjtag_devices() -> Vec<DebugProbeInfo> {
    let Ok(devices) = nusb::list_devices() else {
        return vec![];
    };

    devices
        .filter(is_espjtag_device)
        .map(|device| {
            DebugProbeInfo::new(
                "ESP JTAG".to_string(),
                device.vendor_id(),
                device.product_id(),
                device.serial_number().map(Into::into),
                &EspUsbJtagFactory,
                None,
            )
        })
        .collect()
}
