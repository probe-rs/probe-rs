use std::{cell::RefCell, collections::BTreeMap, io};

use crate::probe::{DebugProbeError, DebugProbeSelector, ProbeCreationError, ProbeError};

use super::{net::GlasgowNetDevice, proto::Target, usb::GlasgowUsbDevice};

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error(
        "Serial number format is \"<device serial>:<IN interface>:<OUT interface>\" or \"tcp:<host>:<port>\"."
    )]
    InvalidFormat,

    #[error("Specified USB interfaces are invalid.")]
    InvalidInterfaces,

    #[error("Could not connect to remote host: {0}.")]
    ConnectionFailed(#[from] io::Error),
}

impl ProbeError for DiscoveryError {}

#[derive(Debug, thiserror::Error)]
pub enum PacketDecodeError {
    #[error("COBS decode error: {0}.")]
    CobsDecodeError(#[from] cobs::DecodeError),

    #[error("Packet too short.")]
    PacketTooShort,

    #[error("Packet destination does not exist.")]
    NoDestination,
}

impl ProbeError for PacketDecodeError {}

fn packet_encode(target: Target, mut data: Vec<u8>) -> Vec<u8> {
    assert!(!data.is_empty());
    data.insert(0, target as u8);
    let mut result = vec![0; cobs::max_encoding_length(data.len())];
    let result_len = cobs::encode(&data, &mut result);
    result.truncate(result_len);
    result
}

fn packet_decode(packet: &mut [u8]) -> Result<(Target, &[u8]), PacketDecodeError> {
    let result_len = cobs::decode_in_place(packet).map_err(PacketDecodeError::CobsDecodeError)?;
    let packet = &packet[..result_len];
    let target = *packet.first().ok_or(PacketDecodeError::PacketTooShort)?;
    let target = Target::try_from(target).map_err(|_| PacketDecodeError::NoDestination)?;
    Ok((target, &packet[1..]))
}

enum GlasgowDeviceInner {
    Usb(GlasgowUsbDevice),
    Net(GlasgowNetDevice),
}

impl GlasgowDeviceInner {
    fn transfer(
        &mut self,
        output: Vec<u8>,
        input: impl FnMut(Vec<u8>) -> Result<bool, DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        match self {
            GlasgowDeviceInner::Usb(usb_device) => usb_device.transfer(output, input),
            GlasgowDeviceInner::Net(tcp_device) => tcp_device.transfer(output, input),
        }
    }
}

pub struct GlasgowDevice {
    inner: RefCell<GlasgowDeviceInner>,
    in_queue: RefCell<Vec<u8>>,
    in_buffers: RefCell<BTreeMap<Target, Vec<u8>>>,
    out_buffers: RefCell<BTreeMap<Target, Vec<u8>>>,
}

impl GlasgowDevice {
    fn new(inner: GlasgowDeviceInner) -> Self {
        Self {
            inner: RefCell::new(inner),
            in_queue: RefCell::new(Vec::new()),
            in_buffers: RefCell::new(BTreeMap::from_iter([
                (Target::Root, Vec::new()),
                (Target::Swd, Vec::new()),
            ])),
            out_buffers: RefCell::new(BTreeMap::from_iter([
                (Target::Root, Vec::new()),
                (Target::Swd, Vec::new()),
            ])),
        }
    }

    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, DebugProbeError> {
        let Some(ref serial) = selector.serial_number else {
            return Err(ProbeCreationError::NotFound.into());
        };

        if serial.starts_with("tcp:") || serial.starts_with("unix:") {
            Ok(Self::new(GlasgowDeviceInner::Net(
                GlasgowNetDevice::new_from_selector(selector)?,
            )))
        } else {
            Ok(Self::new(GlasgowDeviceInner::Usb(
                GlasgowUsbDevice::new_from_selector(selector)?,
            )))
        }
    }

    fn collect_out_data(&self) -> Vec<u8> {
        let mut out_buffers = self.out_buffers.borrow_mut();
        // The (lack of) scheduling in this function has the potential for head-of-line blocking.
        // Right now the use model is straightforward enough this isn't an issue, though.
        let mut output = Vec::new();
        for (&target, buffer) in out_buffers.iter_mut() {
            if !buffer.is_empty() {
                output.extend(packet_encode(target, std::mem::take(buffer)));
                output.push(0x00);
            }
        }
        output
    }

    fn dispatch_in_data(&self, input: Vec<u8>) -> Result<(), PacketDecodeError> {
        let mut in_buffers = self.in_buffers.borrow_mut();
        let mut in_queue = self.in_queue.borrow_mut();
        in_queue.extend(input);
        let mut chunks = in_queue.split_mut(|b| *b == 0x00).collect::<Vec<_>>();
        let chunks_len = chunks.len();
        for chunk in &mut chunks[..chunks_len - 1] {
            let (target, packet) = packet_decode(chunk)?;
            in_buffers
                .get_mut(&target)
                .expect("IN buffer not found")
                .extend(packet);
        }
        *in_queue = chunks[chunks_len - 1].to_vec();
        Ok(())
    }

    pub fn send(&mut self, target: Target, data: &[u8]) {
        tracing::trace!("send({target:?}, {})", hexdump(data));
        self.out_buffers
            .borrow_mut()
            .get_mut(&target)
            .expect("OUT buffer not found")
            .extend(data);
    }

    pub fn recv(&mut self, target: Target, size: usize) -> Result<Vec<u8>, DebugProbeError> {
        tracing::trace!("recv({target:?}, {size}) -> ...");
        let out_transfer = self.collect_out_data();
        self.inner
            .borrow_mut()
            .transfer(out_transfer, |in_transfer| {
                self.dispatch_in_data(in_transfer)?;
                let in_buffers = self.in_buffers.borrow_mut();
                Ok(in_buffers.get(&target).expect("IN buffer not found").len() >= size)
            })?;
        let mut in_buffers = self.in_buffers.borrow_mut();
        let in_buffer = in_buffers.get_mut(&target).expect("IN buffer not found");
        let mut data = in_buffer.split_off(size);
        std::mem::swap(&mut data, in_buffer);
        tracing::trace!("recv({target:?}, {size}) -> {}", hexdump(&data));
        Ok(data)
    }
}

pub(super) fn hexdump(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut result = String::new();
    result.push('<');
    for byte in bytes {
        write!(&mut result, "{byte:02x}").unwrap();
    }
    result.push('>');
    result
}
