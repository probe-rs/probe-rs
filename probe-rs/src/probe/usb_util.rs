//! USB bulk transfer and USB serial port utilities.

use nusb::{
    Endpoint, Interface,
    transfer::{Buffer, Bulk, In, Out},
};
use serialport::{SerialPortType, UsbPortInfo};
use std::fmt::Write;
use std::{io, time::Duration};

/// Encode a usb serial number as a hex
pub(crate) fn to_hex(s: &str) -> String {
    s.as_bytes().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02X}"); // Writing a String never fails
        s
    })
}

/// An extension to write bulk transfers
pub trait BulkWriteExt {
    /// Writes data to the given bulk endpoint from the provided buffer.
    fn write_bulk(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

/// An extension to read bulk transfers
pub trait BulkReadExt {
    /// Reads data from the given bulk endpoint into the provided buffer.
    fn read_bulk(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;
}

impl BulkWriteExt for Endpoint<Bulk, Out> {
    /// Submit a single buffer to a bulk OUT endpoint and wait for it to complete.
    ///
    /// On timeout, the pending transfer is cancelled and drained so the endpoint is
    /// left with no outstanding transfers, and a `TimedOut` error is returned.
    fn write_bulk(&mut self, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let mut transfer_buffer = Buffer::new(buf.len());
        transfer_buffer.extend_from_slice(buf);

        self.submit(transfer_buffer);

        let Some(completion) = self.wait_next_complete(timeout) else {
            // Request cancellation...
            self.cancel_all();

            // ...and then immediately drain the completion. Whether the the response is the
            // result of the original write, the cancellation, or a timeout of the cancellation, we
            // drop it and return a timeout.
            let _ = self.wait_next_complete(Duration::from_millis(100));

            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "bulk write timed out",
            ));
        };

        completion.status.map_err(io::Error::from)?;

        Ok(completion.actual_len)
    }
}

impl BulkReadExt for Endpoint<Bulk, In> {
    /// Submit a read to a bulk IN endpoint and wait for it to complete, copying the
    /// received bytes into `buf`.
    ///
    /// On timeout, the pending transfer is cancelled and drained so the endpoint is
    /// left with no outstanding transfers, and a `TimedOut` error is returned.
    fn read_bulk(&mut self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let max_packet_size = self.max_packet_size().max(1);
        let requested_len = buf.len().div_ceil(max_packet_size) * max_packet_size;

        let transfer_buffer = Buffer::new(requested_len);
        self.submit(transfer_buffer);

        let Some(completion) = self.wait_next_complete(timeout) else {
            // Request cancellation...
            self.cancel_all();

            // ...and then immediately drain the completion. Whether the the response is the
            // result of the original read, the cancellation, or a timeout of the cancellation, we
            // drop it and return a timeout.
            let _ = self.wait_next_complete(Duration::from_millis(100));

            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "bulk read timed out",
            ));
        };

        completion.status.map_err(io::Error::from)?;

        let actual_len = completion.actual_len;
        let data = completion.buffer;

        if actual_len > buf.len() || data.len() > buf.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "device returned {actual_len} bytes, buffer length is {}",
                    buf.len()
                ),
            ));
        }

        buf[..actual_len].copy_from_slice(&data[..actual_len]);

        Ok(actual_len)
    }
}

/// USB bulk transfer utility functions.
///
/// These claim a fresh endpoint for every transfer. For hot paths where the
/// same endpoint is used repeatedly, claim an [`Endpoint`] once and use
/// [`BulkWriteExt::write_bulk`] / [`BulkReadExt::read_bulk`] instead to avoid the
/// per-transfer endpoint setup/teardown cost.
pub trait InterfaceExt {
    /// Reads data from the given bulk endpoint into the provided buffer.
    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize>;

    /// Writes data to the given bulk endpoint from the provided buffer.
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize>;
}

impl InterfaceExt for Interface {
    fn write_bulk(&self, endpoint: u8, buf: &[u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, Out>(endpoint)
            .map_err(io::Error::from)?;

        endpoint.write_bulk(buf, timeout)
    }

    fn read_bulk(&self, endpoint: u8, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        let mut endpoint = self
            .endpoint::<Bulk, In>(endpoint)
            .map_err(io::Error::from)?;

        endpoint.read_bulk(buf, timeout)
    }
}

/// A USB serial port.
#[derive(Clone)]
pub(crate) struct UsbSerialPort {
    /// The OS name of the port, for example `COM3` or `/dev/ttyACM0`.
    pub port_name: String,
    /// The USB information of the port.
    pub info: UsbPortInfo,
}

impl UsbSerialPort {
    /// Returns the USB interface number of the port.
    ///
    /// On macOS, the interface number is sometimes `None`. Then this function takes the number
    /// from the last character of the port name.
    pub fn interface(&self) -> Option<u8> {
        if cfg!(target_os = "macos") && self.info.interface.is_none() {
            return interface_from_port_name(&self.port_name);
        }
        self.info.interface
    }
}

fn interface_from_port_name(port_name: &str) -> Option<u8> {
    let digit = port_name.chars().last()?.to_digit(10)?;
    Some(digit as u8)
}

/// Lists the USB serial ports.
///
/// On macOS, lists only `/dev/cu.*` ports, because each port also has a `/dev/tty.*` copy.
pub(crate) fn usb_serial_ports() -> Result<Vec<UsbSerialPort>, serialport::Error> {
    let ports = serialport::available_ports()?
        .into_iter()
        .filter(|port| !cfg!(target_os = "macos") || port.port_name.contains("/cu."))
        .filter_map(|port| match port.port_type {
            SerialPortType::UsbPort(info) => Some(UsbSerialPort {
                port_name: port.port_name,
                info,
            }),
            _ => {
                tracing::trace!("{}: serial port is not USB -- skipping", port.port_name);
                None
            }
        })
        .collect();

    Ok(ports)
}

/// Selects USB serial ports. A `None` field matches all ports.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct UsbSerialFilter<'a> {
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub serial_number: Option<&'a str>,
    /// The number of the CDC communication interface.
    ///
    /// On macOS, the CDC data interface (this number + 1) also matches, because macOS reports the
    /// data interface.
    pub interface: Option<u8>,
}

impl UsbSerialFilter<'_> {
    /// Returns `true` if `port` matches all fields of the filter.
    pub fn matches(&self, port: &UsbSerialPort) -> bool {
        let info = &port.info;

        if self.vendor_id.is_some_and(|vid| vid != info.vid) {
            return false;
        }
        if self.product_id.is_some_and(|pid| pid != info.pid) {
            return false;
        }
        if self
            .serial_number
            .is_some_and(|sn| info.serial_number.as_deref() != Some(sn))
        {
            return false;
        }
        if let Some(interface) = self.interface {
            let port_interface = port.interface();
            let data_interface = interface
                .checked_add(1)
                .filter(|_| cfg!(target_os = "macos"));
            if port_interface != Some(interface) && port_interface != data_interface {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn port(name: &str, serial_number: Option<&str>, interface: Option<u8>) -> UsbSerialPort {
        UsbSerialPort {
            port_name: name.to_string(),
            info: UsbPortInfo {
                vid: 0x1d50,
                pid: 0x6018,
                serial_number: serial_number.map(str::to_string),
                manufacturer: None,
                product: None,
                interface,
            },
        }
    }

    #[test]
    fn empty_filter_matches_all() {
        assert!(UsbSerialFilter::default().matches(&port("COM3", None, None)));
    }

    #[test]
    fn filter_checks_vid_and_pid() {
        let p = port("COM3", None, Some(0));
        let filter = UsbSerialFilter {
            vendor_id: Some(0x1d50),
            product_id: Some(0x6018),
            ..Default::default()
        };
        assert!(filter.matches(&p));

        let filter = UsbSerialFilter {
            product_id: Some(0x6017),
            ..filter
        };
        assert!(!filter.matches(&p));
    }

    #[test]
    fn filter_checks_serial_number() {
        let filter = UsbSerialFilter {
            serial_number: Some("ABC"),
            ..Default::default()
        };
        assert!(filter.matches(&port("COM3", Some("ABC"), None)));
        assert!(!filter.matches(&port("COM3", Some("XYZ"), None)));
        assert!(!filter.matches(&port("COM3", None, None)));
    }

    #[test]
    fn filter_checks_interface() {
        let macos = cfg!(target_os = "macos");
        for interface in [0, 2] {
            let filter = UsbSerialFilter {
                interface: Some(interface),
                ..Default::default()
            };
            assert!(filter.matches(&port("COM3", None, Some(interface))));
            // Only macOS reports the CDC data interface.
            assert_eq!(
                filter.matches(&port("COM3", None, Some(interface + 1))),
                macos
            );
            assert!(!filter.matches(&port("COM3", None, Some(interface + 2))));
        }

        let filter = UsbSerialFilter {
            interface: Some(255),
            ..Default::default()
        };
        assert!(filter.matches(&port("COM3", None, Some(255))));
        assert!(!filter.matches(&port("COM3", None, Some(0))));
    }

    #[test]
    fn interface_from_name() {
        assert_eq!(
            interface_from_port_name("/dev/cu.usbmodem98B724951"),
            Some(1)
        );
        assert_eq!(interface_from_port_name("/dev/cu.usbmodemX"), None);
        assert_eq!(interface_from_port_name(""), None);
    }
}
