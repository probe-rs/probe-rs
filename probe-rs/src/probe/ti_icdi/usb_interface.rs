use std::fmt::{Debug, Formatter};
use std::io::Write;
use std::time::Duration;

use nusb::{MaybeFuture, descriptors::TransferType, transfer::Direction};

use super::gdb_interface::GdbRemoteInterface;
use super::receive_buffer::ReceiveBuffer;
use super::{IcdiError, IcdiFactory, decode_hex};

use crate::probe::{
    DebugProbeError, DebugProbeInfo, DebugProbeSelector, ProbeCreationError,
    list::{ProbeListItem, usb_probe_accessibility},
    usb_util::InterfaceExt,
};

const ICDI_VID: u16 = 0x1cbe;
const ICDI_PID: u16 = 0x00fd;

const INTERFACE_NR: u8 = 0x02;

pub(super) const ICDI_READ_ENDPOINT: u8 = 0x83;
pub(super) const ICDI_WRITE_ENDPOINT: u8 = 0x02;

pub(super) const TIMEOUT: Duration = Duration::from_secs(1);

pub fn list_icdi_devices() -> Vec<ProbeListItem> {
    let devices = match nusb::list_devices().wait() {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("listing ICDI devices failed: {e}");
            return vec![];
        }
    };

    devices
        .filter(is_icdi_device)
        .map(|device| {
            let info = DebugProbeInfo::new(
                "ICDI",
                device.vendor_id(),
                device.product_id(),
                device.serial_number().map(|s| s.to_string()),
                &IcdiFactory,
                Some(INTERFACE_NR),
                false,
            );
            ProbeListItem {
                info,
                accessibility: usb_probe_accessibility(&device),
            }
        })
        .collect()
}

fn is_icdi_device(device: &nusb::DeviceInfo) -> bool {
    device.vendor_id() == ICDI_VID && device.product_id() == ICDI_PID
}

pub struct IcdiUsbInterface {
    device: nusb::Interface,
    pub serial_number: String,
    max_packet_size: usize,
}

impl Debug for IcdiUsbInterface {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IcdiUsbInterface")
            .field("serial_number", &self.serial_number)
            .field("max_packet_size", &self.max_packet_size)
            .finish_non_exhaustive()
    }
}

impl IcdiUsbInterface {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let device = devices
            .filter(is_icdi_device)
            .find(|device| selector.matches(device))
            .ok_or(ProbeCreationError::NotFound)?;

        let serial_number = device.serial_number().unwrap_or("-").to_string();

        let device = device
            .open()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        let configuration = device
            .configurations()
            .next()
            .ok_or(ProbeCreationError::NotFound)?;

        let interface = configuration
            .interfaces()
            .find(|intf| intf.interface_number() == INTERFACE_NR)
            .ok_or(ProbeCreationError::NotFound)?;

        let altsetting = interface
            .alt_settings()
            .next()
            .ok_or(ProbeCreationError::NotFound)?;

        let mut endpoint_out = false;
        let mut endpoint_in = false;
        for endpoint in altsetting.endpoints() {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }

            match endpoint.direction() {
                Direction::Out if endpoint.address() == ICDI_WRITE_ENDPOINT => {
                    endpoint_out = true;
                }
                Direction::In if endpoint.address() == ICDI_READ_ENDPOINT => {
                    endpoint_in = true;
                }
                _ => {}
            }
        }

        if !endpoint_out || !endpoint_in {
            return Err(IcdiError::EndpointNotFound.into());
        }

        tracing::trace!("Acquired handle for ICDI probe");
        let device_handle = device
            .claim_interface(INTERFACE_NR)
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;

        Ok(Self {
            device: device_handle,
            serial_number,
            max_packet_size: 0x1828,
        })
    }

    pub fn q_supported(&mut self) -> Result<(), DebugProbeError> {
        let buf = self.send_command(b"qSupported")?;
        let resp = std::str::from_utf8(buf.get_payload()?).map_err(|_| IcdiError::Utf8)?;
        for feature in resp.split(';') {
            if let Some(pkt_size) = feature.strip_prefix("PacketSize=") {
                self.max_packet_size =
                    usize::from_str_radix(pkt_size, 16).map_err(|_| IcdiError::HexDecode)?;
                tracing::debug!("Set max packet size to {}", self.max_packet_size);
            }
        }
        Ok(())
    }

    pub fn query_icdi_version(&mut self) -> Result<String, DebugProbeError> {
        let r = self.send_remote_command(b"version")?;
        r.check_cmd_result()?;
        decode_hex(r.get_payload()?)
            .and_then(|mut ascii| {
                while ascii.last() == Some(&b'\n') {
                    ascii.pop();
                }
                String::from_utf8(ascii).map_err(|_| IcdiError::Utf8)
            })
            .map_err(Into::into)
    }

    pub fn set_debug_speed(&mut self, speed_setting: u8) -> Result<(), DebugProbeError> {
        let mut rcmd = Vec::from(&b"debug speed "[..]);
        rcmd.push(speed_setting);
        self.send_remote_command(&rcmd)?.check_cmd_result()
    }

    fn receive_response(&mut self, timeout: Duration) -> Result<Vec<u8>, DebugProbeError> {
        let mut len = 0;
        let mut recv_buf = vec![0u8; self.get_max_packet_size()];
        for _reads in 0..5 {
            let slice = &mut recv_buf[len..];
            len += self
                .device
                .read_bulk(ICDI_READ_ENDPOINT, slice, timeout)
                .map_err(DebugProbeError::Usb)?;
            if len == 0 {
                continue;
            }
            if recv_buf[0] == b'-' {
                // NAK -> retransmission needed
                break;
            }
            if len >= 4 && recv_buf[len - 4] == b'#' && recv_buf[len - 1] == 0 {
                len -= 1; // Remove trailing NUL.
            }
            if len >= 3 && recv_buf[len - 3] == b'#' {
                break;
            }
        }
        recv_buf.truncate(len);
        recv_buf.shrink_to_fit();
        Ok(recv_buf)
    }
}

impl GdbRemoteInterface for IcdiUsbInterface {
    fn get_max_packet_size(&self) -> usize {
        self.max_packet_size
    }

    fn send_packet(&mut self, data: &mut Vec<u8>) -> Result<ReceiveBuffer, DebugProbeError> {
        assert_eq!(data[0], b'$');
        let checksum = data
            .iter()
            .skip(1)
            .fold(0u8, |acc, &byte| acc.wrapping_add(byte));
        write!(data, "#{:02x}", checksum).expect("ICDI buffer write failed.");
        if data.len() > self.get_max_packet_size() {
            return Err(IcdiError::PacketTooLarge.into());
        }
        for _retries in 0..3 {
            let sent = self
                .device
                .write_bulk(ICDI_WRITE_ENDPOINT, data, TIMEOUT)
                .map_err(DebugProbeError::Usb)?;
            if sent != data.len() {
                return Err(IcdiError::IncompleteWrite.into());
            }

            let buf = self.receive_response(TIMEOUT)?;
            if buf.is_empty() {
                return Err(IcdiError::ZeroLengthResponse.into());
            }
            match buf[0] {
                b'-' => {
                    tracing::trace!("Resending packet");
                    continue;
                }
                b'+' => return Ok(ReceiveBuffer::from_vec(buf)),
                _ => {
                    tracing::trace!("Unexpected response from ICDI {buf:?}");
                }
            }
        }
        Err(IcdiError::TooManyRetries.into())
    }
}
