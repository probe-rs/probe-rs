use bitvec::prelude::*;
use nusb::{
    DeviceInfo, Endpoint, MaybeFuture,
    descriptors::TransferType,
    transfer::{Buffer, Bulk, Direction, In, Out},
};
use std::{
    fmt::Debug,
    io,
    time::{Duration, Instant},
};

use probe_rs::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError, ProbeError,
    list::{ProbeListItem, usb_probe_accessibility},
    usb_util::BulkReadExt,
};

use super::EspUsbJtagFactory;

const JTAG_PROTOCOL_CAPABILITIES_VERSION: u8 = 1;
const JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE: u8 = 1;
// The internal repeat counter register is 10 bits. We don't count the initial execution,
// so the maximum repeat counter value is 1023.
const MAX_COMMAND_REPETITIONS: usize = 1023;
// The number of command bytes that the driver collects before it sends a bulk transfer.
// Each command has 4 bits. Thus one byte holds two commands.
//
// This value is much larger than the packet size of 64 bytes. The host controller divides one
// bulk transfer into packets and sends these packets one after the other. The driver does no
// work between two packets. One large transfer therefore removes one round trip through the
// USB stack for each packet.
//
// If the command buffer of the device becomes full, the device sends a NAK. The host controller
// then sends the same data again in the same transfer. The speed of a large transfer is
// therefore the speed of the device.
const OUT_BUFFER_SIZE: usize = 4096;
const IN_EP_BUFFER_SIZE: usize = 64;
const HW_FIFO_SIZE: usize = 4;
// The amount of capture data that the device can hold. If the device has more capture data than
// this, the device stops the command stream and waits for the driver to read the data.
const MAX_IN_FLIGHT_CAPTURE_BITS: usize = (IN_EP_BUFFER_SIZE + HW_FIFO_SIZE) * 8;
// The number of transfers that the driver keeps in the queue of the OUT endpoint.
//
// The driver does not wait for a transfer after it submits the transfer. The host controller can
// therefore start the next transfer immediately after the previous transfer. This removes the
// idle time of one round trip between two transfers.
const MAX_IN_FLIGHT_TRANSFERS: usize = 4;
// The maximum number of JTAG clock cycles in one transfer. This limit makes sure that the device
// completes all the transfers in the queue before `USB_TIMEOUT`.
//
// A command that carries data uses one nibble for each clock cycle. `OUT_BUFFER_SIZE` therefore
// limits a transfer to 2 * OUT_BUFFER_SIZE clock cycles, and this limit has no effect. This
// limit has an effect only for many repeat commands, because a few bytes of repeat commands can
// hold a very large number of clock cycles.
const MAX_BUFFERED_CLOCKS: usize = 16 * 1024;
const USB_TIMEOUT: Duration = Duration::from_millis(500);
const USB_DEVICE_CLASS: u8 = 0xFF;
const USB_DEVICE_SUBCLASS: u8 = 0xFF;
const USB_DEVICE_PROTOCOL: u8 = 0x01;

const USB_VID: u16 = 0x303A;
const USB_PID_BUILTIN_JTAG: u16 = 0x1001;
const USB_PID_BRIDGE: u16 = 0x1002;
const USB_PIDS: &[u16] = &[USB_PID_BUILTIN_JTAG, USB_PID_BRIDGE];

// Built-in USB JTAG uses a vendor-specific descriptor type.
const BUILTIN_CAPS_DESCRIPTOR_TYPE: u8 = 0x20;
const BUILTIN_CAPS_DESCRIPTOR_INDEX: u8 = 0x00;

// ESP-USB-Bridge firmware stores capabilities in a string descriptor.
const BRIDGE_CAPS_DESCRIPTOR_TYPE: u8 = 0x03;
const BRIDGE_CAPS_DESCRIPTOR_INDEX: u8 = 0x0A;

/// Errors that can occur when working with the ESP JTAG adapter.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum EspError {
    /// USB interface or endpoints could not be found.
    InterfaceNotFound,

    /// Unknown capabilities descriptor version: {0:#04x}.
    UnknownCapabilities(u8),

    /// The JTAG reset that starts the connection failed.
    JtagReset(#[source] DebugProbeError),
}

impl ProbeError for EspError {}

pub(super) struct ProtocolHandler {
    // The USB device handle. The driver keeps this handle while it uses the endpoints.
    _device_handle: nusb::Interface,

    /// The command in the queue and their additional repetitions.
    /// For now we do one command at a time.
    command_queue: Option<(Command, usize)>,
    /// The buffer for all commands to be sent to the target. This already contains `repeated`
    /// commands which is the interface's RLE mechanism to reduce the amount of data sent.
    output_buffer: Vec<u8>,
    half_byte_used: bool,
    /// The number of JTAG clock cycles in the commands in `output_buffer`.
    buffered_clocks: usize,
    /// The buffers of the transfers that the OUT endpoint completed. The driver uses them again
    /// for new transfers.
    free_buffers: Vec<Buffer>,
    /// A store for all the read bits (from the target) such that the BitIter the methods return
    /// can borrow and iterate over it.
    response: BitVec,
    pending_in_bits: usize,

    /// The bulk endpoints. The driver claims them one time. A transfer therefore does not have
    /// the cost to open and to close an endpoint.
    ep_out: Endpoint<Bulk, Out>,
    ep_in: Endpoint<Bulk, In>,

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
            .field("base_speed_khz", &self.base_speed_khz)
            .field("div_min", &self.div_min)
            .field("div_max", &self.div_max)
            .finish()
    }
}

impl ProtocolHandler {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let device = devices
            .filter(is_espjtag_device)
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;

        let is_bridge = device.product_id() == USB_PID_BRIDGE;

        let device_handle = device
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        tracing::debug!("Acquired handle for probe");

        let Some(config) = device_handle.configurations().next() else {
            return Err(EspError::InterfaceNotFound.into());
        };

        tracing::debug!("Active config descriptor: {:?}", config);

        let mut found = None;
        for interface in config.interfaces() {
            let interface_number = interface.interface_number();
            tracing::trace!("Interface {interface_number}");

            let Some(descriptor) = interface.alt_settings().next() else {
                continue;
            };

            if !(descriptor.class() == USB_DEVICE_CLASS
                && descriptor.subclass() == USB_DEVICE_SUBCLASS
                && descriptor.protocol() == USB_DEVICE_PROTOCOL)
            {
                tracing::debug!(
                    "Skipping interface {interface_number} because of wrong class/subclass/protocol"
                );
                continue;
            }

            let mut ep_out = None;
            let mut ep_in = None;
            for endpoint in descriptor.endpoints() {
                let address = endpoint.address();
                tracing::trace!("Endpoint {address:#04x}");
                if endpoint.transfer_type() != TransferType::Bulk {
                    tracing::debug!("Skipping endpoint {address:#04x}");
                    continue;
                }

                if endpoint.direction() == Direction::In {
                    ep_in = Some(address);
                } else {
                    ep_out = Some(address);
                }
            }

            if let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) {
                found = Some((interface_number, ep_in, ep_out));
                break;
            }
        }

        let Some((interface_number, ep_in, ep_out)) = found else {
            return Err(EspError::InterfaceNotFound.into());
        };

        tracing::debug!(
            "Claiming interface {interface_number} with IN EP {ep_in} and OUT EP {ep_out}."
        );

        let iface = device_handle
            .claim_interface(interface_number)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        let (caps_type, caps_index) = if is_bridge {
            (BRIDGE_CAPS_DESCRIPTOR_TYPE, BRIDGE_CAPS_DESCRIPTOR_INDEX)
        } else {
            (BUILTIN_CAPS_DESCRIPTOR_TYPE, BUILTIN_CAPS_DESCRIPTOR_INDEX)
        };

        let start = Instant::now();
        let buffer = loop {
            let buffer = device_handle
                .get_descriptor(caps_type, caps_index, 0, USB_TIMEOUT)
                .wait()
                .map_err(|e| ProbeCreationError::Usb(e.into()))?;
            if !buffer.is_empty() {
                break buffer;
            }
            if start.elapsed() > USB_TIMEOUT {
                return Err(ProbeCreationError::Other(
                    "Timeout accessing device descriptor",
                ));
            }
        };

        // String descriptors include a 2-byte header (bLength, bDescriptorType)
        // before the actual capabilities data.
        let buffer = if is_bridge { &buffer[2..] } else { &buffer };

        let protocol_version = buffer[0];
        tracing::trace!("Descriptor bytes: {:02x?}", buffer);
        tracing::debug!("Protocol version: {protocol_version}");
        if protocol_version != JTAG_PROTOCOL_CAPABILITIES_VERSION {
            return Err(EspError::UnknownCapabilities(protocol_version).into());
        }

        let mut base_speed_khz = 1000;
        let mut div_min = 1;
        let mut div_max = 1;

        let length = buffer[1] as usize;
        let mut p = 2usize;
        while p < length {
            let cap_type = buffer[p];
            let cap_length = buffer[p + 1] as usize;
            let cap_bytes = &buffer[p..][..cap_length];

            // cap_length includes the type and length bytes, so we need to skip the first 2.
            let cap_data_bytes = &cap_bytes[2..];

            if cap_type == JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE {
                base_speed_khz =
                    u16::from_le_bytes([cap_data_bytes[0], cap_data_bytes[1]]) as u32 * 10 / 2;
                div_min = u16::from_le_bytes([cap_data_bytes[2], cap_data_bytes[3]]);
                div_max = u16::from_le_bytes([cap_data_bytes[4], cap_data_bytes[5]]);
                tracing::debug!(
                    "Found ESP USB JTAG adapter, base speed is {base_speed_khz}kHz. Available dividers: ({div_min}..{div_max})"
                );
            } else {
                tracing::debug!("Unknown capabilities type {cap_type}");
            }

            p += cap_bytes.len();
        }

        tracing::debug!("Successfully attached to ESP USB JTAG.");

        let ep_out = iface
            .endpoint::<Bulk, Out>(ep_out)
            .map_err(|e| ProbeCreationError::Usb(io::Error::from(e)))?;
        let ep_in = iface
            .endpoint::<Bulk, In>(ep_in)
            .map_err(|e| ProbeCreationError::Usb(io::Error::from(e)))?;

        let mut this = Self {
            _device_handle: iface,
            command_queue: None,
            output_buffer: Vec::with_capacity(OUT_BUFFER_SIZE),
            half_byte_used: false,
            buffered_clocks: 0,
            free_buffers: Vec::new(),
            response: BitVec::new(),
            ep_out,
            ep_in,
            pending_in_bits: 0,

            base_speed_khz,
            div_min,
            div_max,
        };

        this.sync_capture_stream().map_err(|error| match error {
            DebugProbeError::Usb(error) => ProbeCreationError::Usb(error),
            DebugProbeError::ProbeCouldNotBeCreated(error) => error,
            other => EspError::JtagReset(other).into(),
        })?;

        Ok(this)
    }

    /// Discard leftover capture data and put the TAP in Test-Logic-Reset.
    ///
    /// Two capture lengths are used so that a leftover packet of the first size cannot satisfy
    /// the second check.
    fn sync_capture_stream(&mut self) -> Result<(), DebugProbeError> {
        const PING_A_BITS: usize = 16;
        const PING_B_BITS: usize = 24;
        const FOLLOW_UP_TIMEOUT: Duration = Duration::from_millis(50);

        self.discard_until_exact_packet(PING_A_BITS, FOLLOW_UP_TIMEOUT)?;
        self.discard_until_exact_packet(PING_B_BITS, FOLLOW_UP_TIMEOUT)?;

        self.pending_in_bits = 0;
        self.response.clear();

        Ok(())
    }

    fn discard_until_exact_packet(
        &mut self,
        capture_bits: usize,
        follow_up_timeout: Duration,
    ) -> Result<(), DebugProbeError> {
        let expected_bytes = capture_bits.div_ceil(8);
        const MAX_PINGS: usize = 8;
        const MAX_PACKETS_PER_PING: usize = 16;

        for _ in 0..MAX_PINGS {
            self.ping_capture(capture_bits)?;

            let mut saw_packet = false;
            for _ in 0..MAX_PACKETS_PER_PING {
                let timeout = if saw_packet {
                    follow_up_timeout
                } else {
                    USB_TIMEOUT
                };

                match self.read_in_packet(timeout)? {
                    Some(n) if n == expected_bytes => return Ok(()),
                    Some(_) => saw_packet = true,
                    None if saw_packet => break,
                    None => {
                        return Err(DebugProbeError::Usb(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "USB JTAG adapter did not return capture data",
                        )));
                    }
                }
            }
        }

        Err(DebugProbeError::Timeout)
    }

    fn ping_capture(&mut self, capture_bits: usize) -> Result<(), DebugProbeError> {
        self.pending_in_bits = 0;
        self.response.clear();

        for _ in 0..capture_bits {
            self.shift_bit(true, true, true)?;
        }

        self.finalize_previous_command()?;
        self.add_raw_command(Command::Flush)?;
        if self.half_byte_used {
            self.push_raw_command(Command::Flush);
        }
        self.send_buffer()?;
        self.complete_writes(0)?;
        self.pending_in_bits = 0;

        Ok(())
    }

    fn read_in_packet(&mut self, timeout: Duration) -> Result<Option<usize>, DebugProbeError> {
        let mut incoming = [0; IN_EP_BUFFER_SIZE];
        match self.ep_in.read_bulk(&mut incoming, timeout) {
            Ok(0) => Ok(None),
            Ok(n) => Ok(Some(n)),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => Ok(None),
            Err(error) => Err(DebugProbeError::Usb(error)),
        }
    }

    /// Put a bit on TDI and possibly read one from TDO.
    /// to receive the bytes from this operations call [`ProtocolHandler::flush`]
    ///
    /// Note that if the internal buffer is exceeded bytes will be automatically flushed to usb device
    pub fn shift_bit(&mut self, tms: bool, tdi: bool, cap: bool) -> Result<(), DebugProbeError> {
        if cap && self.pending_in_bits >= MAX_IN_FLIGHT_CAPTURE_BITS {
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

        self.push_command(Command::Clock { cap, tdi, tms })?;

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
    fn push_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        assert!(matches!(command, Command::Clock { .. }));
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

        // The endpoint reports a write error only at the completion of the transfer. Wait for all
        // the transfers, because the caller must know that the commands are on the device.
        self.complete_writes(0)?;

        Ok(())
    }

    /// Flushes pending commands and reads the captured bits from the target.
    ///
    /// This method returns the response buffer and clears it. The returned buffer will contain
    /// all bits captured since the last call to `read_captured_bits`.
    pub(super) fn read_captured_bits(&mut self) -> Result<BitVec, DebugProbeError> {
        self.flush()?;

        Ok(std::mem::take(&mut self.response))
    }

    /// Writes a command one or multiple times into the raw buffer we send to the USB EP later
    /// or if the out buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn write_stream(
        &mut self,
        command: Command,
        repetitions: usize,
    ) -> Result<(), DebugProbeError> {
        tracing::trace!("add raw cmd {:?} reps={}", command, repetitions + 1);

        // The capture buffer of the device can be too full for the new capture data. If this
        // condition occurs, send the commands that are in the output buffer. Then read the capture
        // data to make space for the new capture data.
        if command.captures() && self.pending_in_bits + repetitions + 1 > MAX_IN_FLIGHT_CAPTURE_BITS
        {
            self.send_buffer()?;

            // One command can make more capture data than the limit permits. Then read all the
            // available capture data.
            while self.pending_in_bits > 0
                && self.pending_in_bits + repetitions + 1 > MAX_IN_FLIGHT_CAPTURE_BITS
            {
                self.receive_buffer()?;
            }
        }

        // Send the actual command.
        self.add_raw_command(command)?;
        self.add_repetitions(repetitions)?;

        if command.captures() {
            // Only increment pending bits if a whole command is in the buffer.
            self.pending_in_bits += repetitions + 1;
        }

        if matches!(command, Command::Clock { .. }) {
            self.buffered_clocks += repetitions + 1;

            // Limit the time that the device needs for one transfer.
            if self.buffered_clocks >= MAX_BUFFERED_CLOCKS {
                self.send_buffer_if_unpadded()?;
            }
        }

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
        // If we reach a maximal size of the output buffer, we flush.
        if self.output_buffer.len() == OUT_BUFFER_SIZE && !self.half_byte_used {
            self.send_buffer()?;
        }

        self.push_raw_command(command);

        Ok(())
    }

    fn push_raw_command(&mut self, command: Command) {
        let command = u8::from(command);
        if self.half_byte_used {
            // We have to add the lower nibble of the command to the last byte in the buffer.
            let last_byte = unsafe {
                // SAFETY: half_byte_used means we have at least 4 bits in the buffer
                self.output_buffer.last_mut().unwrap_unchecked()
            };
            *last_byte |= command;
            self.half_byte_used = false;
        } else {
            // We have to add a new byte to the buffer.
            self.output_buffer.push(command << 4);
            self.half_byte_used = true;
        }
    }

    /// Sends the commands in the output buffer to the USB EP. Sends the commands only if the
    /// driver can do this without a fill nibble. Returns `true` if the driver sent the buffer.
    ///
    /// A fill nibble makes the driver read all the capture data. Refer to `send_buffer`. A caller
    /// that only limits the size of a transfer must wait for the next command, because the next
    /// command completes the byte.
    fn send_buffer_if_unpadded(&mut self) -> Result<bool, DebugProbeError> {
        if self.half_byte_used {
            return Ok(false);
        }

        self.send_buffer()?;

        Ok(true)
    }

    /// Sends the commands stored in the output buffer to the USB EP.
    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        // A fill nibble also makes the device complete the current capture byte. The device then
        // adds bits to the response, but the driver does not count these bits. Therefore read all
        // the capture data. The additional bits are then at the end of the data, and
        // `receive_buffer` removes them.
        let padded = self.half_byte_used;

        if padded {
            // Make sure we add an additional nibble to the command buffer if the number of
            // nibbles is odd, as we cannot send a standalone nibble.
            self.push_raw_command(Command::Flush);
        }

        tracing::trace!("Writing {} bytes to usb endpoint", self.output_buffer.len());

        if !self.output_buffer.is_empty() {
            // Keep space in the queue for the new transfer.
            self.complete_writes(MAX_IN_FLIGHT_TRANSFERS - 1)?;

            let mut buffer = self
                .free_buffers
                .pop()
                .unwrap_or_else(|| self.ep_out.allocate(OUT_BUFFER_SIZE));
            buffer.clear();
            buffer.extend_from_slice(&self.output_buffer);

            self.ep_out.submit(buffer);
        }

        self.output_buffer.clear();
        self.buffered_clocks = 0;

        if padded {
            while self.pending_in_bits != 0 {
                self.receive_buffer()?;
            }
        }

        // If there's more than a bufferful of data queuing up in the jtag adapters IN endpoint, empty all but one buffer.
        while self.pending_in_bits > MAX_IN_FLIGHT_CAPTURE_BITS {
            self.receive_buffer()?;
        }

        Ok(())
    }

    /// Waits until the OUT endpoint has not more than `keep_pending` transfers in the queue.
    ///
    /// The endpoint reports the result of a transfer at this point, and not at the submission.
    /// The driver can therefore not relate an error to a specific command.
    fn complete_writes(&mut self, keep_pending: usize) -> Result<(), DebugProbeError> {
        while self.ep_out.pending() > keep_pending {
            let Some(completion) = self.ep_out.wait_next_complete(USB_TIMEOUT) else {
                self.cancel_writes();

                return Err(DebugProbeError::Usb(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "bulk write timed out",
                )));
            };

            if let Err(error) = completion.status {
                self.cancel_writes();

                return Err(DebugProbeError::Usb(io::Error::from(error)));
            }

            let sent = completion.actual_len;
            let requested = completion.buffer.len();
            if sent != requested {
                self.cancel_writes();

                return Err(DebugProbeError::Usb(io::Error::other(format!(
                    "the device accepted {sent} of {requested} command bytes"
                ))));
            }

            self.free_buffers.push(completion.buffer);
        }

        Ok(())
    }

    /// Removes all the transfers from the queue of the OUT endpoint.
    fn cancel_writes(&mut self) {
        self.ep_out.cancel_all();

        while self.ep_out.pending() > 0 {
            if self.ep_out.wait_next_complete(USB_TIMEOUT).is_none() {
                break;
            }
        }
    }

    /// Tries to receive pending in bits from the USB EP.
    fn receive_buffer(&mut self) -> Result<(), DebugProbeError> {
        tracing::trace!("Receiving buffer, pending bits: {}", self.pending_in_bits);

        if self.pending_in_bits == 0 {
            return Ok(());
        }

        let count = self.pending_in_bits.div_ceil(8).min(IN_EP_BUFFER_SIZE);
        let mut incoming = [0; IN_EP_BUFFER_SIZE];

        let read_bytes = self
            .ep_in
            .read_bulk(&mut incoming, USB_TIMEOUT)
            .map_err(|e| {
                tracing::warn!(
                    "Something went wrong in read_bulk {:?} when trying to read {}bytes - pending_in_bits: {}",
                    e,
                    count,
                    self.pending_in_bits,
                );
                DebugProbeError::Usb(e)
            })?;

        if read_bytes > count {
            tracing::warn!("Read more bytes than expected: {} > {}", read_bytes, count);
        }

        let bits_in_buffer = self.pending_in_bits.min(read_bytes * 8);
        let incoming = &incoming[..read_bytes];

        tracing::trace!("Read: {:?}, length = {}", incoming, bits_in_buffer);
        self.pending_in_bits = self.pending_in_bits.saturating_sub(bits_in_buffer);

        self.response
            .extend_from_bitslice(&incoming.view_bits::<Lsb0>()[..bits_in_buffer]);

        Ok(())
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
enum Command {
    Clock { cap: bool, tdi: bool, tms: bool },
    Reset(bool),
    Flush,
    Repeat(u8),
}

impl Command {
    fn captures(&self) -> bool {
        matches!(self, Command::Clock { cap, .. } if *cap)
    }
}

impl From<Command> for u8 {
    fn from(command: Command) -> Self {
        match command {
            Command::Clock { cap, tdi, tms } => {
                (u8::from(cap) << 2) | (u8::from(tms) << 1) | u8::from(tdi)
            }
            Command::Reset(srst) => 8 | u8::from(srst),
            Command::Flush => 0xA,
            Command::Repeat(repetitions) => 0xC + repetitions,
        }
    }
}

pub(super) fn is_espjtag_device(device: &DeviceInfo) -> bool {
    // Check the VID/PID.
    device.vendor_id() == USB_VID && USB_PIDS.contains(&device.product_id())
}

#[tracing::instrument(skip_all)]
pub(super) fn list_espjtag_devices() -> Vec<ProbeListItem> {
    match nusb::list_devices().wait() {
        Ok(devices) => devices
            .filter(is_espjtag_device)
            .map(|device| ProbeListItem {
                accessibility: usb_probe_accessibility(&device),
                info: DebugProbeInfo::new(
                    "ESP JTAG".to_string(),
                    device.vendor_id(),
                    device.product_id(),
                    device.serial_number().map(Into::into),
                    &EspUsbJtagFactory,
                    None,
                    false,
                ),
            })
            .collect(),
        Err(e) => {
            tracing::warn!("error listing ESP USB JTAG devices: {e}");
            vec![]
        }
    }
}
