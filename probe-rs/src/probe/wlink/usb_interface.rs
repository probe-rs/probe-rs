use std::time::Duration;

use nusb::{MaybeFuture, descriptors::TransferType, transfer::Direction};

use crate::probe::{DebugProbeError, DebugProbeSelector, ProbeCreationError};

use super::{WchLinkError, commands::WchLinkCommand, get_wlink_info};

/// Bulk USB transport behind [`WchLinkUsbDevice`].
///
/// The indirection exists so native-flashing failure paths (short writes,
/// zero reads, unexpected acknowledgements) can be tested without hardware;
/// production uses the nusb interface directly.
pub(crate) trait UsbBulkTransport: Send {
    fn write_bulk(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> std::io::Result<usize>;
    fn read_bulk(
        &mut self,
        endpoint: u8,
        buf: &mut [u8],
        timeout: Duration,
    ) -> std::io::Result<usize>;
}

impl UsbBulkTransport for nusb::Interface {
    fn write_bulk(
        &mut self,
        endpoint: u8,
        data: &[u8],
        timeout: Duration,
    ) -> std::io::Result<usize> {
        crate::probe::usb_util::InterfaceExt::write_bulk(self, endpoint, data, timeout)
    }
    fn read_bulk(
        &mut self,
        endpoint: u8,
        buf: &mut [u8],
        timeout: Duration,
    ) -> std::io::Result<usize> {
        crate::probe::usb_util::InterfaceExt::read_bulk(self, endpoint, buf, timeout)
    }
}

pub(crate) const ENDPOINT_OUT: u8 = 0x01;
pub(crate) const ENDPOINT_IN: u8 = 0x81;

/// Bulk endpoints used for firmware/loader blob streaming during native flashing.
pub(crate) const DATA_ENDPOINT_OUT: u8 = 0x02;
pub(crate) const DATA_ENDPOINT_IN: u8 = 0x82;

/// USB timeout for native flash data transfers and long-running flash commands.
///
/// The regular command timeout below is too short for whole-chip erases and
/// multi-kilobyte bulk streaming; this matches the 5 s timeout `wlink` uses.
const FLASH_USB_TIMEOUT: Duration = Duration::from_millis(5000);

// const RAW_ENDPOINT_OUT: u8 = 0x02;
// const RAW_ENDPOINT_IN: u8 = 0x82;

pub struct WchLinkUsbDevice {
    device_handle: Box<dyn UsbBulkTransport>,
    /// Whether the data endpoints for native flashing (`0x02`/`0x82`)
    /// were found. Older firmware may only expose the command endpoints;
    /// those probes keep debugging over DMI but cannot use native flashing.
    has_data_endpoints: bool,
}

#[cfg(test)]
impl WchLinkUsbDevice {
    /// Build a device around a scripted transport for failure-path tests.
    pub(crate) fn for_test(transport: Box<dyn UsbBulkTransport>, has_data_endpoints: bool) -> Self {
        Self {
            device_handle: transport,
            has_data_endpoints,
        }
    }
}

impl WchLinkUsbDevice {
    /// Whether the interface exposes the bulk data endpoints for native flashing.
    pub(crate) fn has_data_endpoints(&self) -> bool {
        self.has_data_endpoints
    }
}

/// One scripted USB step for failure-path tests.
#[cfg(test)]
#[derive(Debug)]
pub(crate) enum UsbScriptStep {
    /// Expect a bulk write of exact bytes; report `written` bytes accepted.
    Write {
        endpoint: u8,
        expect: Vec<u8>,
        written: usize,
    },
    /// Reply to a bulk read with these bytes, or a scripted I/O error.
    Read {
        endpoint: u8,
        reply: Result<Vec<u8>, std::io::ErrorKind>,
    },
}

/// Scripted [`UsbBulkTransport`]: replays expectations in order, panics on
/// unexpected traffic, and reports leftover steps via `assert_finished`
/// (e.g. proving cleanup traffic was sent after a failure).
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct ScriptedUsb {
    steps: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<UsbScriptStep>>>,
}

#[cfg(test)]
impl ScriptedUsb {
    pub(crate) fn new(steps: Vec<UsbScriptStep>) -> Self {
        Self {
            steps: std::sync::Arc::new(std::sync::Mutex::new(steps.into())),
        }
    }

    /// Assert every scripted step was consumed.
    pub(crate) fn assert_finished(&self) {
        let steps = self.steps.lock().unwrap();
        assert!(steps.is_empty(), "unused USB script steps: {steps:?}");
    }
}

#[cfg(test)]
impl UsbBulkTransport for ScriptedUsb {
    fn write_bulk(
        &mut self,
        endpoint: u8,
        data: &[u8],
        _timeout: Duration,
    ) -> std::io::Result<usize> {
        match self.steps.lock().unwrap().pop_front() {
            Some(UsbScriptStep::Write {
                endpoint: want,
                expect,
                written,
            }) => {
                assert_eq!(endpoint, want, "USB write endpoint mismatch");
                assert_eq!(data, &expect[..], "USB write payload mismatch");
                Ok(written)
            }
            other => panic!("expected USB write, script has {other:?}"),
        }
    }

    fn read_bulk(
        &mut self,
        endpoint: u8,
        buf: &mut [u8],
        _timeout: Duration,
    ) -> std::io::Result<usize> {
        match self.steps.lock().unwrap().pop_front() {
            Some(UsbScriptStep::Read {
                endpoint: want,
                reply,
            }) => {
                assert_eq!(endpoint, want, "USB read endpoint mismatch");
                let reply =
                    reply.map_err(|kind| std::io::Error::new(kind, "scripted USB error"))?;
                let len = reply.len().min(buf.len());
                buf[..len].copy_from_slice(&reply[..len]);
                Ok(len)
            }
            other => panic!("expected USB read, script has {other:?}"),
        }
    }
}

impl WchLinkUsbDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let devices = nusb::list_devices()
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        let device = devices
            .filter(|device| selector.matches(device))
            .find(|device| get_wlink_info(device).is_some())
            .ok_or(ProbeCreationError::NotFound)?;

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
            .find(|intf| intf.interface_number() == 0)
            .ok_or(ProbeCreationError::NotFound)?;

        let altsetting = interface
            .alt_settings()
            .next()
            .ok_or(ProbeCreationError::NotFound)?;

        let mut endpoint_out = None;
        let mut endpoint_in = None;
        let mut data_endpoint_out = None;
        let mut data_endpoint_in = None;
        for endpoint in altsetting.endpoints() {
            if endpoint.transfer_type() != TransferType::Bulk {
                continue;
            }

            match endpoint.direction() {
                Direction::Out if endpoint.address() == ENDPOINT_OUT => {
                    endpoint_out = Some(endpoint.address());
                }
                Direction::In if endpoint.address() == ENDPOINT_IN => {
                    endpoint_in = Some(endpoint.address());
                }
                Direction::Out if endpoint.address() == DATA_ENDPOINT_OUT => {
                    data_endpoint_out = Some(endpoint.address());
                }
                Direction::In if endpoint.address() == DATA_ENDPOINT_IN => {
                    data_endpoint_in = Some(endpoint.address());
                }
                _ => {}
            }
        }

        if endpoint_out.is_none() || endpoint_in.is_none() {
            return Err(WchLinkError::EndpointNotFound.into());
        }
        let has_data_endpoints = data_endpoint_out.is_some() && data_endpoint_in.is_some();
        if !has_data_endpoints {
            tracing::warn!(
                "WCH-Link interface has no bulk data endpoints; native flashing is unavailable"
            );
        }

        tracing::trace!("Acquired handle for probe");
        let device_handle = device
            .claim_interface(interface.interface_number())
            .wait()
            .map_err(|e| ProbeCreationError::Usb(e.into()))?;
        tracing::trace!("Claimed interface 0 of USB device.");

        let usb_wlink = Self {
            device_handle: Box::new(device_handle),
            has_data_endpoints,
        };

        tracing::debug!("Successfully attached to WCH-Link.");

        Ok(usb_wlink)
    }

    pub(crate) fn send_command<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
    ) -> Result<C::Response, DebugProbeError> {
        self.send_command_timeout(cmd, Duration::from_millis(100))
    }

    /// Send a command with an explicit USB timeout.
    ///
    /// Native flash operations (chip erase, bulk streaming handshakes) can
    /// take seconds, far beyond the default 100 ms DMI timeout.
    pub(crate) fn send_command_timeout<C: WchLinkCommand + std::fmt::Debug>(
        &mut self,
        cmd: C,
        timeout: Duration,
    ) -> Result<C::Response, DebugProbeError> {
        tracing::trace!("Sending command: {:?}", cmd);

        let mut rxbuf = [0u8; 64];
        let len = cmd.to_bytes(&mut rxbuf)?;

        let written_bytes = self
            .device_handle
            .write_bulk(ENDPOINT_OUT, &rxbuf[..len], timeout)
            .map_err(DebugProbeError::Usb)?;

        if written_bytes != len {
            return Err(WchLinkError::NotEnoughBytesWritten {
                is: written_bytes,
                should: len,
            }
            .into());
        }

        let mut rxbuf = [0u8; 64];
        let read_bytes = self
            .device_handle
            .read_bulk(ENDPOINT_IN, &mut rxbuf[..], timeout)
            .map_err(DebugProbeError::Usb)?;

        if read_bytes < 3 {
            return Err(WchLinkError::NotEnoughBytesRead {
                is: read_bytes,
                should: 3,
            }
            .into());
        }
        if read_bytes != rxbuf[2] as usize + 3 {
            return Err(WchLinkError::NotEnoughBytesRead {
                is: read_bytes,
                should: 3 + (rxbuf[2] as usize),
            }
            .into());
        }

        let response = cmd.parse_response(&rxbuf[..read_bytes])?;

        Ok(response)
    }

    /// Write a bulk packet to the data endpoint (native flashing).
    ///
    /// Used to upload the flash loader blob and to stream firmware chunks.
    /// Unlike [`Self::send_command`], this uses the flash-appropriate timeout
    /// and has no response handling.
    pub(crate) fn write_data_bulk(&mut self, data: &[u8]) -> Result<(), DebugProbeError> {
        let mut written_total = 0;
        while written_total < data.len() {
            let written = self
                .device_handle
                .write_bulk(DATA_ENDPOINT_OUT, &data[written_total..], FLASH_USB_TIMEOUT)
                .map_err(DebugProbeError::Usb)?;
            if written == 0 {
                break;
            }
            written_total += written;
        }

        if written_total != data.len() {
            return Err(WchLinkError::NotEnoughBytesWritten {
                is: written_total,
                should: data.len(),
            }
            .into());
        }

        Ok(())
    }

    /// Read exactly `len` bytes from the data endpoint (native flashing).
    ///
    /// Reads in 64-byte USB packets until `len` bytes were received, mirroring
    /// `wlink`'s `read_data`. Used for fastprogram chunk acknowledgements and
    /// memory readback.
    pub(crate) fn read_data_bulk(&mut self, len: usize) -> Result<Vec<u8>, DebugProbeError> {
        let mut buf = Vec::with_capacity(len);
        while buf.len() < len {
            let mut chunk = [0u8; 64];
            let read = self
                .device_handle
                .read_bulk(DATA_ENDPOINT_IN, &mut chunk, FLASH_USB_TIMEOUT)
                .map_err(DebugProbeError::Usb)?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
        }

        if buf.len() != len {
            return Err(WchLinkError::NotEnoughBytesRead {
                is: buf.len(),
                should: len,
            }
            .into());
        }

        Ok(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_write_reports_not_enough_bytes() {
        let script = ScriptedUsb::new(vec![UsbScriptStep::Write {
            endpoint: DATA_ENDPOINT_OUT,
            expect: vec![0x01, 0x02, 0x03],
            written: 0,
        }]);
        let mut device = WchLinkUsbDevice::for_test(Box::new(script), true);
        let error = device.write_data_bulk(&[0x01, 0x02, 0x03]).unwrap_err();
        assert!(
            matches!(error, DebugProbeError::ProbeSpecific(_)),
            "unexpected error: {error:?}"
        );
    }

    #[test]
    fn zero_read_reports_not_enough_bytes() {
        let script = ScriptedUsb::new(vec![UsbScriptStep::Read {
            endpoint: DATA_ENDPOINT_IN,
            reply: Ok(vec![]),
        }]);
        let mut device = WchLinkUsbDevice::for_test(Box::new(script), true);
        let error = device.read_data_bulk(4).unwrap_err();
        assert!(
            matches!(error, DebugProbeError::ProbeSpecific(_)),
            "unexpected error: {error:?}"
        );
    }
}
