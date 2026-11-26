use std::fmt::Display;

use std::time::Duration;

use postcard_schema::Schema;
use serde::{Deserialize, Serialize};

use crate::file::ServerPath;
use crate::{Key, RpcResult, Session};

// Separate from DebugProbeInfo because we can't serialize a &dyn ProbeFactory
#[derive(Debug, Serialize, Deserialize, Clone, Schema, PartialEq)]
pub struct DebugProbeEntry {
    /// The name of the debug probe.
    pub identifier: String,
    /// How the probe is reached.
    pub transport: ProbeTransport,

    pub probe_type: String,

    /// The probe was found but the current user cannot access its device
    /// (e.g. a missing udev rule on Linux).
    pub inaccessible: bool,
}

/// How a debug probe is reached. See `probe_rs::probe::ProbeTransport`.
#[derive(Debug, Serialize, Deserialize, Clone, Schema, PartialEq)]
pub enum ProbeTransport {
    /// A USB probe, or for now a probe that is not on USB but is listed with a placeholder VID and
    /// PID. See `probe_rs::probe::ProbeTransport::Usb`.
    Usb(UsbProbeEntry),
    /// A probe reached over TCP.
    Tcp(TcpAddress),
}

/// A probe reached over TCP. See `probe_rs::probe::TcpAddress`.
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq, Schema)]
pub struct TcpAddress {
    pub driver: String,
    pub address: String,
}

impl Display for TcpAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tcp://{}@{}", self.driver, self.address)
    }
}

/// A USB debug probe. See `probe_rs::probe::UsbProbeInfo`.
#[derive(Debug, Serialize, Deserialize, Clone, Schema, PartialEq)]
pub struct UsbProbeEntry {
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The interface of the debug probe.
    pub interface: Option<u8>,
    /// The serial number of the debug probe.
    pub serial_number: String,

    /// The USB port, if known. See `probe_rs::probe::UsbProbeInfo::usb_port`.
    pub usb_port: Option<UsbPort>,
}

impl Display for ProbeTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProbeTransport::Usb(usb) => {
                write!(f, "{:04x}:{:04x}", usb.vendor_id, usb.product_id)?;
                if let Some(interface) = usb.interface {
                    write!(f, "-{interface}")?;
                }
                write!(f, ":{}", usb.serial_number)
            }
            ProbeTransport::Tcp(address) => address.fmt(f),
        }
    }
}

impl Display for DebugProbeEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} -- {} ({})",
            self.identifier, self.transport, self.probe_type
        )?;
        if let ProbeTransport::Usb(UsbProbeEntry {
            usb_port: Some(port),
            ..
        }) = &self.transport
        {
            write!(f, " usb-busid://{port}")?;
        }
        Ok(())
    }
}

impl DebugProbeEntry {
    pub fn selector(&self) -> DebugProbeSelector {
        match &self.transport {
            ProbeTransport::Usb(usb) => DebugProbeSelector::Usb(UsbSelector {
                vendor_id: usb.vendor_id,
                product_id: usb.product_id,
                serial_number: Some(usb.serial_number.clone()),
                interface: usb.interface,
                usb_port: usb.usb_port.clone(),
            }),
            ProbeTransport::Tcp(address) => DebugProbeSelector::Tcp(address.clone()),
        }
    }
}

pub type ListProbesResponse = RpcResult<Vec<DebugProbeEntry>>;

#[derive(Debug, Serialize, Deserialize, Schema)]
pub struct SelectProbeRequest {
    pub probe: Option<DebugProbeSelector>,
}

#[derive(Debug, Serialize, Deserialize, Schema)]
pub enum SelectProbeResult {
    Success(DebugProbeEntry),
    MultipleProbes(Vec<DebugProbeEntry>),
}

pub type SelectProbeResponse = RpcResult<SelectProbeResult>;

#[derive(Serialize, Deserialize, Schema)]
pub enum AttachResult<T = Key<Session>> {
    Success(T),
    ProbeNotFound,
    FailedToOpenProbe(String),
    ProbeInUse,
    /// The probe opened, but attaching to the chip failed.
    TargetAttachFailed {
        message: String,
        connect_under_reset: bool,
    },
}

/// Which  selection sequence to use when cJTAG is selected.
#[derive(Debug, docsplay::Display, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum CJtagActivation {
    /// Long-form sequence
    Long,
    /// Short-form sequence
    Short,
}

#[derive(Debug, docsplay::Display, PartialEq, Eq, Clone, Copy, Serialize, Deserialize, Schema)]
pub enum WireProtocol {
    /// JTAG
    Jtag,
    /// SWD
    Swd,
    /// cJTAG (IEEE 1149.7 OScan1)
    CJtag(CJtagActivation),
}

/// Describes the way a probe should be selected. See `probe_rs::probe::DebugProbeSelector`.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub enum DebugProbeSelector {
    /// A USB probe, or for now a probe that is not on USB but is selected with a placeholder VID
    /// and PID. See `probe_rs::probe::DebugProbeSelector::Usb`.
    Usb(UsbSelector),
    /// A probe reached over TCP.
    Tcp(TcpAddress),
    /// The USB probe plugged into this port.
    UsbPort(UsbPort),
}

/// The USB port a device is plugged into. See `probe_rs::probe::UsbPort`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct UsbPort {
    pub bus_id: String,
    pub ports: Vec<u8>,
}

impl Display for UsbPort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-", self.bus_id)?;
        for (i, port) in self.ports.iter().enumerate() {
            if i > 0 {
                f.write_str(".")?;
            }
            write!(f, "{port}")?;
        }
        Ok(())
    }
}

/// Selects a USB probe. See `probe_rs::probe::UsbSelector`.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct UsbSelector {
    /// The the USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The the USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The the interface of the debug probe to be used.
    pub interface: Option<u8>,
    /// The the serial number of the debug probe to be used.
    pub serial_number: Option<String>,
    /// The USB port, if known. See `probe_rs::probe::UsbProbeInfo::usb_port`.
    pub usb_port: Option<UsbPort>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct AttachRequest {
    pub chip: Option<String>,
    /// The image to run or flash. Attach falls back to its chip when it detects none, and fails if
    /// the image is built for a different chip than the target.
    pub image: Option<ServerPath>,
    pub protocol: Option<WireProtocol>,
    pub probe: DebugProbeEntry,
    pub speed: Option<u32>,
    pub connect_under_reset: bool,
    pub allow_erase_all: bool,
    pub resume_target: bool,
    pub wait_for_probe: Option<Duration>,
    /// Open the serial port to the target.
    pub target_serial: bool,
}

pub type AttachResponse = RpcResult<AttachResult>;
