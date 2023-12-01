use std::{fmt::Debug, time::Duration};

use bitvec::{prelude::*, slice::BitSlice, vec::BitVec};
use rusb::{request_type, Context, Device, Direction, TransferType, UsbContext};

use crate::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, DebugProbeType, ProbeCreationError,
};

const JTAG_PROTOCOL_CAPABILITIES_VERSION: u8 = 1;
const JTAG_PROTOCOL_CAPABILITIES_SPEED_APB_TYPE: u8 = 1;
const MAX_COMMAND_REPETITIONS: usize = 1024;
const OUT_BUFFER_SIZE: usize = OUT_EP_BUFFER_SIZE * 32;
const OUT_EP_BUFFER_SIZE: usize = 128;
const IN_EP_BUFFER_SIZE: usize = 64;
const HW_FIFO_SIZE: usize = 4;
const USB_TIMEOUT: Duration = Duration::from_millis(5000);
const USB_DEVICE_CLASS: u8 = 0xFF;
const USB_DEVICE_SUBCLASS: u8 = 0xFF;
const USB_DEVICE_PROTOCOL: u8 = 0x01;
const USB_DEVICE_TRANSFER_TYPE: TransferType = TransferType::Bulk;

const USB_CONFIGURATION: u8 = 0x0;

const USB_VID: u16 = 0x303A;
const USB_PID: u16 = 0x1001;

const VENDOR_DESCRIPTOR_JTAG_CAPABILITIES: u16 = 0x2000;

pub(super) struct ProtocolHandler {
    // The USB device handle.
    device_handle: rusb::DeviceHandle<rusb::Context>,

    // The command in the queue and their repetitions.
    // For now we do one command at a time.
    command_queue: Option<(Command, usize)>,
    // The buffer for all commands to be sent to the target. This already contains `repeated` commands which are basically
    // a mechanism to compress the datastream by adding a `Repeat` command to repeat the previous command `n` times instead of
    // actually putting the command into the queue `n` times.
    output_buffer: Vec<Command>,
    // A store for all the read bits (from the target) such that the BitIter the methods return can borrow and iterate over it.
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
    pub fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Self, ProbeCreationError> {
        let selector = selector.into();

        let context = Context::new()?;

        tracing::debug!("Acquired libusb context.");

        let device = context
            .devices()?
            .iter()
            .filter(is_espjtag_device)
            .find_map(|device| {
                let descriptor = device.device_descriptor().ok()?;
                // First match the VID & PID.
                if selector.vendor_id == descriptor.vendor_id()
                    && selector.product_id == descriptor.product_id()
                {
                    // If the VID & PID match, match the serial if one was given.
                    if let Some(serial) = &selector.serial_number {
                        let sn_str = read_serial_number(&device, &descriptor).ok();
                        if sn_str.as_ref() == Some(serial) {
                            Some(device)
                        } else {
                            None
                        }
                    } else {
                        // If no serial was given, the VID & PID match is enough; return the device.
                        Some(device)
                    }
                } else {
                    None
                }
            })
            .map_or(Err(ProbeCreationError::NotFound), Ok)?;

        let mut device_handle = device.open()?;

        tracing::debug!("Aquired handle for probe");

        let config = device.config_descriptor(USB_CONFIGURATION)?;

        tracing::debug!("Active config descriptor: {:?}", &config);

        let descriptor = device.device_descriptor()?;

        tracing::debug!("Device descriptor: {:?}", &descriptor);

        let mut ep_out = None;
        let mut ep_in = None;

        for interface in config.interfaces() {
            tracing::trace!("Interface {}", interface.number());
            let descriptor = interface.descriptors().next();
            if let Some(descriptor) = descriptor {
                if descriptor.class_code() == USB_DEVICE_CLASS
                    && descriptor.sub_class_code() == USB_DEVICE_SUBCLASS
                    && descriptor.protocol_code() == USB_DEVICE_PROTOCOL
                {
                    for endpoint in descriptor.endpoint_descriptors() {
                        tracing::trace!("Endpoint {}: {}", endpoint.number(), endpoint.address());
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
                    interface.number(),
                    ep_in,
                    ep_out
                );
                device_handle.claim_interface(interface.number())?;
            }
        }

        if let (Some(_), Some(_)) = (ep_in, ep_out) {
        } else {
            return Err(ProbeCreationError::ProbeSpecific(
                "USB interface or endpoints could not be found.".into(),
            ));
        }

        let mut buffer = [0; 255];
        device_handle.read_control(
            request_type(
                rusb::Direction::In,
                rusb::RequestType::Standard,
                rusb::Recipient::Device,
            ),
            rusb::constants::LIBUSB_REQUEST_GET_DESCRIPTOR,
            VENDOR_DESCRIPTOR_JTAG_CAPABILITIES,
            0,
            &mut buffer,
            USB_TIMEOUT,
        )?;

        let mut base_speed_khz = 1000;
        let mut div_min = 1;
        let mut div_max = 1;

        let protocol_version = buffer[0];
        tracing::debug!("{:?}", &buffer[..20]);
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
                tracing::info!("Found ESP USB JTAG adapter, base speed is {base_speed_khz}khz. Available dividers: ({div_min}..{div_max})");
            } else {
                tracing::warn!("Unknown capabilities type {:01X?}", typ);
            }

            p += length as usize;
        }

        tracing::debug!("Succesfully attached to ESP USB JTAG.");

        Ok(Self {
            device_handle,
            command_queue: None,
            output_buffer: Vec::new(),
            response: BitVec::new(),
            // The following expects are okay as we check that the values we call them on are `Some`.
            ep_out: ep_out.expect("This is a bug. Please report it."),
            ep_in: ep_in.expect("This is a bug. Please report it."),
            pending_in_bits: 0,

            base_speed_khz,
            div_min,
            div_max,
        })
    }

    /// Put a bit on TDI and possibly read one from TDO.
    pub fn jtag_io(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: bool,
    ) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        self.jtag_io_async(tms, tdi, cap)?;
        self.flush()
    }

    /// Put a bit on TDI and possibly read one from TDO.
    /// to receive the bytes from this operations call [`ProtocolHandler::flush`]
    ///
    /// Note that if the internal buffer is exceeded bytes will be automatically flushed to usb device
    pub fn jtag_io_async(
        &mut self,
        tms: impl IntoIterator<Item = bool>,
        tdi: impl IntoIterator<Item = bool>,
        cap: bool,
    ) -> Result<(), DebugProbeError> {
        tracing::debug!("JTAG IO! {} ", cap);
        for (tms, tdi) in tms.into_iter().zip(tdi.into_iter()) {
            self.push_command(Command::Clock { cap, tdi, tms })?;
            if cap {
                self.pending_in_bits += 1;
            }
        }
        Ok(())
    }

    /// Sets the two different resets on the target.
    /// NOTE: Only `srst` can be set for now. Setting `trst` is not implemented yet.
    pub fn set_reset(&mut self, _trst: bool, srst: bool) -> Result<(), DebugProbeError> {
        // TODO: Handle trst using setup commands. This is not necessarily required and can be left as is for the moiment..
        self.push_command(Command::Reset(srst))?;
        self.flush()?;
        Ok(())
    }

    /// Adds a command to the command queue.
    /// This will properly add repeat commands if possible.
    fn push_command(&mut self, command: Command) -> Result<(), DebugProbeError> {
        if let Some((command_in_queue, repetitions)) = self.command_queue.as_mut() {
            if command == *command_in_queue && *repetitions < MAX_COMMAND_REPETITIONS {
                *repetitions += 1;
                return Ok(());
            } else {
                let command = *command_in_queue;
                let repetitions = *repetitions;
                self.write_stream(command, repetitions)?;
            }
        }

        self.command_queue = Some((command, 1));

        Ok(())
    }

    /// Flushes all the pending commands to the JTAG adapter.
    pub fn flush(&mut self) -> Result<BitVec<u8, Lsb0>, DebugProbeError> {
        if let Some((command_in_queue, repetitions)) = self.command_queue.take() {
            self.write_stream(command_in_queue, repetitions)?;
        }

        tracing::debug!("Flushing ...");

        self.add_raw_command(Command::Flush)?;

        // Make sure we add an additional nibble to the command buffer if the number of nibbles is odd,
        // as we cannot send a standalone nibble.
        if self.output_buffer.len() % 2 == 1 {
            self.add_raw_command(Command::Flush)?;
        }

        self.send_buffer()?;

        while self.pending_in_bits != 0 {
            self.receive_buffer()?;
        }

        Ok(std::mem::replace(&mut self.response, BitVec::new()))
    }

    /// Writes a command one or multiple times into the raw buffer we send to the USB EP later
    /// or if the out buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn write_stream(
        &mut self,
        command: impl Into<Command>,
        repetitions: usize,
    ) -> Result<(), DebugProbeError> {
        let command = command.into();
        let mut repetitions = repetitions;
        tracing::trace!("add raw cmd {:?} reps={}", command, repetitions);

        // Make sure we send flush commands only once and not repeated (Could make the target unhapy).
        if command == Command::Flush {
            repetitions = 1;
        }

        // Send the actual command.
        self.add_raw_command(command)?;

        // We already sent the command once so we need to do one less repetition.
        repetitions -= 1;

        // Send repetitions as many times as required.
        // We only send 2 bits with each repetition command as per the protocol.
        while repetitions > 0 {
            self.add_raw_command(Command::Repetitions((repetitions & 3) as u8))?;
            repetitions >>= 2;
        }

        Ok(())
    }

    /// Adds a single command to the output buffer and writes it to the USB EP if the buffer reaches a limit of `OUT_BUFFER_SIZE`.
    fn add_raw_command(&mut self, command: impl Into<Command>) -> Result<(), DebugProbeError> {
        let command = command.into();
        self.output_buffer.push(command);

        // If we reach a maximal size of the output buffer, we flush.
        if self.output_buffer.len() == OUT_BUFFER_SIZE {
            self.send_buffer()?;
        }

        // Undocumented condition to flush buffer.
        // First check, whether the output buffer is suitable for flushing? it should be modulo of the EP buffer size
        if self.output_buffer.len() % OUT_EP_BUFFER_SIZE == 0 {
            // Second check, if it is suitable, is there enough to flush
            if self.pending_in_bits > (IN_EP_BUFFER_SIZE + HW_FIFO_SIZE) * 8 {
                self.send_buffer()?;
            }
        }

        Ok(())
    }

    /// Sends the commands stored in the output buffer to the USB EP.
    fn send_buffer(&mut self) -> Result<(), DebugProbeError> {
        let commands = self
            .output_buffer
            .chunks(2)
            .map(|chunk| {
                if chunk.len() == 2 {
                    let unibble: u8 = chunk[0].into();
                    let lnibble: u8 = chunk[1].into();
                    (unibble << 4) | lnibble
                } else {
                    chunk[0].into()
                }
            })
            .collect::<Vec<_>>();

        tracing::trace!(
            "Writing {}byte ({}nibles) to usb endpoint",
            commands.len(),
            commands.len() * 2
        );
        let mut offset = 0;
        let mut total = 0;
        loop {
            let bytes = self
                .device_handle
                .write_bulk(self.ep_out, &commands[offset..], USB_TIMEOUT)
                .map_err(|e| DebugProbeError::Usb(Some(Box::new(e))))?;
            total += bytes;
            offset += bytes;

            if total == commands.len() {
                break;
            }
        }

        // We only clear the output buffer on a successful transmission of all bytes.
        self.output_buffer.clear();

        // If there's more than a bufferful of data queing up in the jtag adapters IN endpoint, empty all but one buffer.
        loop {
            if self.pending_in_bits > (IN_EP_BUFFER_SIZE + HW_FIFO_SIZE) * 8 {
                self.receive_buffer()?;
            } else {
                break;
            }
        }

        Ok(())
    }

    /// Tries to receive pending in bits from the USB EP.
    fn receive_buffer(&mut self) -> Result<(), DebugProbeError> {
        let count = ((self.pending_in_bits + 7) / 8).min(IN_EP_BUFFER_SIZE);
        let mut incoming = vec![0; count];

        tracing::trace!("Receiving buffer, pending bits: {}", self.pending_in_bits);

        if count == 0 {
            return Ok(());
        }

        let mut offset = 0;
        let mut total = 0;
        loop {
            let read_bytes = self
                .device_handle
                .read_bulk(self.ep_in, &mut incoming[offset..], USB_TIMEOUT)
                .map_err(|e| {
                    tracing::warn!(
                        "Something went wrong in read_bulk {:?} when trying to read {}bytes - pending_in_bits: {}",
                        e,
                        count,
                        self.pending_in_bits,
                    );
                    DebugProbeError::Usb(Some(Box::new(e)))
                })?;
            total += read_bytes;
            offset += read_bytes;

            if read_bytes == 0 {
                tracing::debug!("Read 0 bytes from USB");
                return Ok(());
            }

            if total == count {
                break;
            } else {
                tracing::warn!("USB only recieved {} out of {} bytes", read_bytes, count);
            }

            tracing::trace!("Received {} bytes.", read_bytes);
        }

        let bits_in_buffer = self.pending_in_bits.min(total * 8);

        tracing::trace!("Read: {:?}, length = {}", incoming, bits_in_buffer);
        self.pending_in_bits -= bits_in_buffer;

        let bs: &BitSlice<_, Lsb0> = BitSlice::from_slice(&incoming);
        self.response.extend_from_bitslice(&bs[..bits_in_buffer]);

        Ok(())
    }
}

#[derive(PartialEq, Debug, Clone, Copy)]
pub(super) enum Command {
    Clock { cap: bool, tdi: bool, tms: bool },
    Reset(bool),
    Flush,
    // TODO: What is this?
    _Rsvd,
    Repetitions(u8),
}

impl From<Command> for u8 {
    fn from(command: Command) -> Self {
        match command {
            Command::Clock { cap, tdi, tms } => {
                (if cap { 4 } else { 0 } | if tms { 2 } else { 0 } | u8::from(tdi))
            }
            Command::Reset(srst) => 8 | u8::from(srst),
            Command::Flush => 0xA,
            Command::_Rsvd => 0xB,
            Command::Repetitions(repetitions) => 0xC + repetitions,
        }
    }
}

/// Try to read the serial number of a USB device.
fn read_serial_number<T: rusb::UsbContext>(
    device: &rusb::Device<T>,
    descriptor: &rusb::DeviceDescriptor,
) -> Result<String, rusb::Error> {
    let timeout = Duration::from_millis(100);

    let handle = device.open()?;
    let language = handle
        .read_languages(timeout)?
        .get(0)
        .cloned()
        .ok_or(rusb::Error::BadDescriptor)?;
    handle.read_serial_number_string(language, descriptor, timeout)
}

pub(super) fn is_espjtag_device<T: UsbContext>(device: &Device<T>) -> bool {
    // Check the VID/PID.
    if let Ok(descriptor) = device.device_descriptor() {
        descriptor.vendor_id() == USB_VID && descriptor.product_id() == USB_PID
    } else {
        false
    }
}

#[tracing::instrument(skip_all)]
pub fn list_espjtag_devices() -> Vec<DebugProbeInfo> {
    rusb::Context::new()
        .and_then(|context| context.devices())
        .map_or(vec![], |devices| {
            devices
                .iter()
                .filter(is_espjtag_device)
                .filter_map(|device| {
                    let descriptor = device.device_descriptor().ok()?;

                    let sn_str = match read_serial_number(&device, &descriptor) {
                        Ok(serial_number) => Some(serial_number),
                        Err(e) => {
                            // Reading the serial number can fail, e.g. if the driver for the probe
                            // is not installed. In this case we can still list the probe,
                            // just without serial number.
                            tracing::debug!(
                                "Failed to read serial number of device {:04x}:{:04x} : {}",
                                descriptor.vendor_id(),
                                descriptor.product_id(),
                                e
                            );
                            tracing::debug!("This might be happening because of a missing driver.");
                            None
                        }
                    };

                    Some(DebugProbeInfo::new(
                        "ESP JTAG".to_string(),
                        descriptor.vendor_id(),
                        descriptor.product_id(),
                        sn_str,
                        DebugProbeType::EspJtag,
                        None,
                    ))
                })
                .collect::<Vec<_>>()
        })
}
