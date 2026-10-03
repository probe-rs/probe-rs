//! Texas Instruments ICDI (In-Circuit Debug Interface) probe support.
//!
//! ICDI is the debug probe built into many TI evaluation boards. It speaks a
//! GDB remote protocol over USB bulk endpoints and does not expose DAP or SWO.

mod arm;
mod gdb_interface;
mod receive_buffer;
mod usb_interface;

use std::sync::Arc;

use crate::{
    Error,
    architecture::arm::{ArmDebugInterface, ArmError, sequences::ArmDebugSequence},
    probe::{
        DebugProbe, DebugProbeError, DebugProbeSelector, ProbeError, ProbeFactory, WireProtocol,
        list::ProbeListItem, ti_icdi::arm::IcdiArmDebug,
    },
};

use gdb_interface::GdbRemoteInterface;
use usb_interface::IcdiUsbInterface;

/// Errors specific to the TI ICDI probe.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub(crate) enum IcdiError {
    /// Malformed ICDI response.
    MalformedResponse,
    /// Empty ICDI response payload.
    EmptyPayload,
    /// ICDI command returned error {0}.
    CommandError(u8),
    /// Failed to decode hex data from an ICDI response.
    HexDecode,
    /// ICDI response was not valid UTF-8.
    Utf8,
    /// ICDI packet was larger than the negotiated maximum size.
    PacketTooLarge,
    /// ICDI USB write was incomplete.
    IncompleteWrite,
    /// ICDI returned a zero-length response.
    ZeroLengthResponse,
    /// Too many retries while sending an ICDI packet.
    TooManyRetries,
    /// Short memory read from ICDI.
    ShortRead,
    /// ICDI USB endpoint not found.
    EndpointNotFound,
    /// `OK:` prefix missing from an ICDI memory read response.
    MissingOkPrefix,
}

impl ProbeError for IcdiError {}

/// Factory for creating [`IcdiProbe`] probes.
#[derive(Debug)]
pub struct IcdiFactory;

impl std::fmt::Display for IcdiFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TI ICDI")
    }
}

impl ProbeFactory for IcdiFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        tracing::debug!("Opening TI ICDI: {selector:?}");
        let mut device = IcdiUsbInterface::new_from_selector(selector)?;
        let ver = device.query_icdi_version()?;
        let name = format!("ICDI S/N: {}, ver: {}", device.serial_number, ver);

        Ok(Box::new(IcdiProbe {
            device,
            protocol: WireProtocol::Jtag,
            name,
            speed_khz: 2000,
            speed_setting: b'0',
        }))
    }

    fn list_probes(&self) -> Vec<ProbeListItem> {
        usb_interface::list_icdi_devices()
    }
}

/// A Texas Instruments ICDI debug probe.
#[derive(Debug)]
pub struct IcdiProbe {
    device: IcdiUsbInterface,
    protocol: WireProtocol,
    name: String,
    speed_khz: u32,
    speed_setting: u8,
}

impl DebugProbe for IcdiProbe {
    fn get_name(&self) -> &str {
        &self.name
    }

    fn speed_khz(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        /*
        > 750 kHz -> 0 (0)
        > 300 kHz -> 1 (1)
        > 200 kHz -> 2 (5)
        > 150 kHz -> 3 (10)
        <= 150 kHz -> 4 (20)
         */
        if !(91..=6000).contains(&speed_khz) {
            return Err(DebugProbeError::UnsupportedSpeed(speed_khz));
        }
        self.speed_setting = match speed_khz {
            91..=150 => b'4',
            151..=200 => b'3',
            201..=300 => b'2',
            301..=750 => b'1',
            _ => b'0',
        };
        self.device.set_debug_speed(self.speed_setting)?;
        self.speed_khz = speed_khz;

        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("attach({:?})", self.protocol);
        self.device.set_debug_speed(self.speed_setting)?;
        self.device.q_supported()?;
        // Enable extended mode.
        self.device.send_command(b"!")?.check_cmd_result()
    }

    fn detach(&mut self) -> Result<(), Error> {
        tracing::debug!("Detaching from TI-ICDI.");
        self.device
            .send_remote_command(b"debug disable")
            .and_then(|r| r.check_cmd_result())?;
        Ok(())
    }

    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.device
            .send_remote_command(b"debug hreset")?
            .check_cmd_result()
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        self.device
            .send_remote_command(b"debug sreset")
            .and_then(|r| r.check_cmd_result())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        self.device
            .send_remote_command(b"debug hreset")
            .and_then(|r| r.check_cmd_result())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Jtag => {
                self.protocol = protocol;
                Ok(())
            }
            _ => Err(DebugProbeError::UnsupportedProtocol(protocol)),
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        Some(self.protocol)
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn try_get_arm_debug_interface<'probe>(
        self: Box<Self>,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Box<dyn ArmDebugInterface + 'probe>, (Box<dyn DebugProbe>, ArmError)> {
        Ok(Box::new(IcdiArmDebug::new(self, sequence)))
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

/// Decode a hex-encoded ASCII byte slice into raw bytes.
pub(super) fn decode_hex(input: &[u8]) -> Result<Vec<u8>, IcdiError> {
    if !input.len().is_multiple_of(2) {
        return Err(IcdiError::HexDecode);
    }
    let mut out = Vec::with_capacity(input.len() / 2);
    for chunk in input.as_chunks::<2>().0 {
        let s = std::str::from_utf8(chunk).map_err(|_| IcdiError::HexDecode)?;
        out.push(u8::from_str_radix(s, 16).map_err(|_| IcdiError::HexDecode)?);
    }
    Ok(out)
}
