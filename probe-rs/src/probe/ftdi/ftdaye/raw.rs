use std::{
    collections::VecDeque,
    io::{self, Read, Write},
    time::Duration,
};

use nusb::{
    transfer::{Control, ControlType, Direction, EndpointType, Recipient},
    DeviceInfo,
};
use tracing::{debug, trace, warn};

use crate::{probe::usb_util::InterfaceExt, DebugProbeError};

use super::{error::FtdiError, FtdiDriver, Interface, Result};

/// An FTDI driver using "raw" communications with nusb
pub struct FtdiRaw {
    /// USB device handle
    handle: nusb::Interface,

    interface: Interface,

    usb_read_timeout: Duration,
    usb_write_timeout: Duration,

    read_queue: VecDeque<u8>,
    read_buffer: Box<[u8]>,
    max_packet_size: usize,
}

trait Ep {
    fn read_ep(self) -> u8;
    fn write_ep(self) -> u8;
}

impl Ep for Interface {
    fn read_ep(self) -> u8 {
        match self {
            Interface::A => 0x81,
            Interface::B => 0x83,
            Interface::C => 0x85,
            Interface::D => 0x87,
        }
    }

    fn write_ep(self) -> u8 {
        match self {
            Interface::A => 0x02,
            Interface::B => 0x04,
            Interface::C => 0x06,
            Interface::D => 0x08,
        }
    }
}

impl FtdiDriver for FtdiRaw {
    fn usb_reset(&mut self) -> Result<()> {
        const SIO_RESET_REQUEST: u8 = 0;
        const SIO_RESET_SIO: u16 = 0;

        self.sio_write(SIO_RESET_REQUEST, SIO_RESET_SIO)
    }

    fn usb_purge_buffers(&mut self) -> Result<()> {
        self.usb_purge_tx_buffer()?;
        self.usb_purge_rx_buffer()?;

        Ok(())
    }

    fn set_usb_timeouts(&mut self, read_timeout: Duration, write_timeout: Duration) -> Result<()> {
        self.usb_read_timeout = read_timeout;
        self.usb_write_timeout = write_timeout;

        Ok(())
    }

    fn set_latency_timer(&mut self, value: u8) -> Result<()> {
        const SIO_SET_LATENCY_TIMER_REQUEST: u8 = 0x09;

        self.sio_write(SIO_SET_LATENCY_TIMER_REQUEST, value as u16)
    }

    fn set_bitmode(&mut self, bitmask: u8, mode: super::BitMode) -> Result<()> {
        const SIO_SET_BITMODE_REQUEST: u8 = 0x0B;

        self.sio_write(
            SIO_SET_BITMODE_REQUEST,
            u16::from_le_bytes([bitmask, mode as u8]),
        )?;

        Ok(())
    }

    fn read_data(&mut self, mut data: &mut [u8]) -> io::Result<usize> {
        let mut total = 0;
        while !data.is_empty() {
            // Move data out of the read queue
            if !self.read_queue.is_empty() {
                let read = self.read_queue.read(data).unwrap();
                tracing::debug!("Copied {} bytes from queue", read);

                data = &mut data[read..];
                total += read;
            }

            // Read from USB
            if !data.is_empty() {
                let read = self.handle.read_bulk(
                    self.interface.read_ep(),
                    &mut self.read_buffer,
                    self.usb_read_timeout,
                )?;

                tracing::debug!("Read {:02x?} bytes from USB", &self.read_buffer[..read]);

                if read <= 2 {
                    // No more data to read.
                    break;
                }

                let (status, read_data) = self.read_buffer[..read].split_at(2);

                tracing::debug!("Status: {:02X?} [{} data]", status, read);

                let copy = read_data.len().min(data.len());
                let (to_buffer, to_save) = read_data.split_at(copy);

                if copy > 0 {
                    data[..copy].copy_from_slice(to_buffer);
                    data = &mut data[copy..];
                    tracing::debug!("Copied {} bytes from USB", copy);
                    total += copy;
                }

                if !to_save.is_empty() {
                    tracing::debug!("Queued {} bytes from USB", to_save.len());
                    self.read_queue.extend(to_save);
                    break;
                }
            }
        }

        tracing::debug!("read {} bytes", total);

        Ok(total)
    }

    fn write_data(&mut self, data: &[u8]) -> io::Result<usize> {
        let mut total = 0;
        for chunk in data.chunks(self.max_packet_size) {
            total +=
                self.handle
                    .write_bulk(self.interface.write_ep(), chunk, self.usb_write_timeout)?;
        }

        tracing::debug!("wrote {} bytes", total);

        Ok(total)
    }
}

impl FtdiRaw {
    pub fn open(usb_device: &DeviceInfo, interface: Interface) -> Result<Self, DebugProbeError> {
        fn open_error(e: std::io::Error, while_: &'static str) -> DebugProbeError {
            let help = if cfg!(windows) {
                "(this error may be caused by not having a driver installed; install the official FTDI drivers or use Zadig (https://zadig.akeo.ie/) to install WinUSB)"
            } else {
                ""
            };

            DebugProbeError::Usb(std::io::Error::other(format!(
                "error while {while_}: {e}{help}",
            )))
        }

        let handle = usb_device
            .open()
            .map_err(|e| open_error(e, "opening the USB device"))?;

        let configs: Vec<_> = handle.configurations().collect();

        let conf = &configs[0];
        if configs.len() != 1 {
            warn!("device has {} configurations, expected 1", configs.len());

            if configs.len() > 1 {
                let configuration = handle
                    .active_configuration()
                    .map_err(FtdiError::ActiveConfigurationError)?
                    .configuration_value();

                if configuration != conf.configuration_value() {
                    handle
                        .set_configuration(conf.configuration_value())
                        .map_err(FtdiError::Usb)?;
                }
            }
        }

        debug!("scanning {} interfaces", conf.interfaces().count());
        trace!("active configuration descriptor: {:#x?}", conf);

        let mut usb_interface = None;

        // Try to find the specified interface
        for intf in conf.interfaces() {
            trace!("interface #{} descriptors:", intf.interface_number());

            for descr in intf.alt_settings() {
                trace!("{:#x?}", descr);

                let endpoints: Vec<_> = descr.endpoints().collect();
                trace!("endpoint descriptors: {:#x?}", endpoints);

                if endpoints
                    .iter()
                    .any(|ep| ep.transfer_type() != EndpointType::Bulk)
                {
                    warn!(
                        "encountered non-bulk endpoints, skipping interface: {:#x?}",
                        endpoints
                    );
                    continue;
                }

                let endpoint_count = endpoints.len();
                let Ok::<[_; 2], _>([read_ep, write_ep]) = endpoints.try_into() else {
                    warn!(
                        "skipping interface with {} endpoints, expected 2",
                        endpoint_count
                    );
                    continue;
                };

                let (read_ep, write_ep) = if read_ep.direction() == Direction::In {
                    (read_ep, write_ep)
                } else {
                    (write_ep, read_ep)
                };

                if read_ep.address() != interface.read_ep()
                    || write_ep.address() != interface.write_ep()
                {
                    warn!(
                        "interface {} does not match requested interface {:?}",
                        descr.interface_number(),
                        interface
                    );
                    continue;
                }

                if let Some((intf, _)) = usb_interface {
                    Err(FtdiError::Other(format!(
                        "found multiple matching USB interfaces ({} and {})",
                        intf,
                        descr.interface_number()
                    )))?
                }

                usb_interface = Some((descr.interface_number(), write_ep.max_packet_size()));
                debug!("Interface is #{}", descr.interface_number());
            }
        }

        let Some((intf, max_packet_size)) = usb_interface else {
            Err(FtdiError::Other("device is not a FTDI device".to_string()))?
        };

        let handle = handle
            .claim_interface(intf)
            .map_err(|e| open_error(e, "taking control over USB device"))?;

        Ok(Self {
            handle,
            interface,
            usb_read_timeout: Duration::from_secs(5),
            usb_write_timeout: Duration::from_secs(5),
            read_queue: VecDeque::new(),
            read_buffer: vec![0; max_packet_size].into_boxed_slice(),
            max_packet_size,
        })
    }

    fn sio_write(&mut self, request: u8, value: u16) -> Result<()> {
        let result = self
            .handle
            .control_out_blocking(
                Control {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request,
                    value,
                    index: self.interface.index(),
                },
                &[],
                self.usb_write_timeout,
            )
            .map_err(std::io::Error::from)?;

        tracing::debug!("Response to {:02X}/{:04X}: {:?}", request, value, result);

        Ok(())
    }

    /// Clears the write buffer on the chip.
    fn usb_purge_tx_buffer(&mut self) -> Result<()> {
        const SIO_RESET_REQUEST: u8 = 0;
        const SIO_RESET_PURGE_TX: u16 = 2;

        self.sio_write(SIO_RESET_REQUEST, SIO_RESET_PURGE_TX)
    }

    fn usb_purge_rx_buffer(&mut self) -> Result<()> {
        const SIO_RESET_REQUEST: u8 = 0;
        const SIO_RESET_PURGE_RX: u16 = 1;

        self.sio_write(SIO_RESET_REQUEST, SIO_RESET_PURGE_RX)?;

        self.read_queue.clear();

        Ok(())
    }
}

impl Read for FtdiRaw {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.read_data(buf)
    }
}

impl Write for FtdiRaw {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_data(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
