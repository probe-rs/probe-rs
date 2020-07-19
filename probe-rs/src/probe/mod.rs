pub(crate) mod daplink;
pub(crate) mod jlink;
pub(crate) mod stlink;

use crate::architecture::arm::{DAPAccess, PortType};
use crate::config::{RegistryError, TargetSelector};
use crate::error::Error;
use crate::{Memory, Session};
use jlink::list_jlink_devices;
use std::{convert::TryFrom, fmt};
use thiserror::Error;

#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum WireProtocol {
    Swd,
    Jtag,
}

impl fmt::Display for WireProtocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            WireProtocol::Swd => write!(f, "SWD"),
            WireProtocol::Jtag => write!(f, "JTAG"),
        }
    }
}

impl std::str::FromStr for WireProtocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "swd" => Ok(WireProtocol::Swd),
            "jtag" => Ok(WireProtocol::Jtag),
            _ => Err(format!(
                "'{}' is not a valid protocol. Choose from [swd, jtag].",
                s
            )),
        }
    }
}

/// A command queued in a batch for later execution
///
/// Mostly used internally but returned in DebugProbeError to indicate
/// which batched command actually encountered the error.
#[derive(Copy, Clone, Debug)]
pub enum BatchCommand {
    Read(PortType, u16),
    Write(PortType, u16, u32),
}

impl fmt::Display for BatchCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BatchCommand::Read(port, addr) => write!(f, "Read(port={:?}, addr={})", port, addr),
            BatchCommand::Write(port, addr, data) => write!(
                f,
                "Write(port={:?}, addr={}, data=0x{:08x}",
                port, addr, data
            ),
        }
    }
}

#[derive(Error, Debug)]
pub enum DebugProbeError {
    #[error("USB Communication Error")]
    USB(#[source] Option<Box<dyn std::error::Error + Send + Sync>>),
    #[error("The firmware on the probe is outdated")]
    ProbeFirmwareOutdated,
    #[error("An error specific to a probe type occured")]
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Probe could not be created")]
    ProbeCouldNotBeCreated(#[from] ProbeCreationError),
    #[error("Probe does not support protocol")]
    UnsupportedProtocol(WireProtocol),
    // TODO: This is core specific, so should probably be moved there.
    #[error("Operation timed out")]
    Timeout,
    #[error("An error specific to the selected architecture occured")]
    ArchitectureSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("The connected probe does not support the interface '{0}'")]
    InterfaceNotAvailable(&'static str),
    #[error("An error occured while working with the registry occured")]
    Registry(#[from] RegistryError),
    #[error("Tried to close interface while it was still in use")]
    InterfaceInUse,
    #[error("The requested speed setting ({0} kHz) is not supported by the probe")]
    UnsupportedSpeed(u32),
    #[error("You need to be attached to the target to perform this action")]
    NotAttached,
    #[error("You need to be detached from the target to perform this action")]
    Attached,
    #[error("Some functionality was not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("Error in previous batched command")]
    BatchError(BatchCommand),
    #[error("Command not supported by probe")]
    CommandNotSupportedByProbe,
    #[error("Unable to set hardware breakpoint, all available breakpoint units are in use.")]
    BreakpointUnitsExceeded,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(Error, Debug)]
pub enum ProbeCreationError {
    #[error("Probe was not found.")]
    NotFound,
    #[error("USB device could not be opened. Please check the permissions.")]
    CouldNotOpen,
    #[error("{0}")]
    HidApi(#[from] hidapi::HidError),
    #[error("{0}")]
    Rusb(#[from] rusb::Error),
    #[error("An error specific to a probe type occured: {0}")]
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("{0}")]
    Other(&'static str),
}

/// The Probe struct is a generic wrapper over the different
/// probes supported.
///
/// # Examples
///
/// ## Open the first probe found
///
/// The `list_all` and `from_probe_info` functions can be used
/// to create a new `Probe`:
///
/// ```no_run
/// use probe_rs::Probe;
///
/// let probe_list = Probe::list_all();
/// let probe = Probe::open(&probe_list[0]);
/// ```
#[derive(Debug)]
pub struct Probe {
    inner: Box<dyn DebugProbe>,
    attached: bool,
}

impl Probe {
    pub fn new(probe: impl DebugProbe + 'static) -> Self {
        Self {
            inner: Box::new(probe),
            attached: false,
        }
    }

    /// Get a list of all debug probes found.
    /// This can be used to select the debug probe which
    /// should be used.
    pub fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = daplink::tools::list_daplink_devices();
        list.extend(stlink::tools::list_stlink_devices());

        list.extend(list_jlink_devices().expect("Failed to list J-Link devices."));

        list
    }

    /// Create a `Probe` from `DebugProbeInfo`. Use the
    /// `Probe::list_all()` function to get the information
    /// about all probes available.
    pub fn open(selector: impl Into<DebugProbeSelector> + Clone) -> Result<Self, DebugProbeError> {
        match daplink::DAPLink::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match stlink::STLink::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match jlink::JLink::new_from_selector(selector) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    pub fn from_specific_probe(probe: Box<dyn DebugProbe>) -> Self {
        Probe {
            inner: probe,
            attached: false,
        }
    }

    // /// Tries to mass erase a locked nRF52 chip, this process may timeout, if it does, the chip
    // /// might be unlocked or not, it is advised to try again if flashing fails
    // pub fn nrf_recover(&mut self) -> Result<(), DebugProbeError> {
    //     let ctrl_port = match get_ap_by_idr(self, |idr| idr == CTRL_AP_IDR) {
    //         Some(port) => CtrlAP::from(port),
    //         None => {
    //             return Err(DebugProbeError::AccessPortError(
    //                 AccessPortError::CtrlAPNotFound,
    //             ));
    //         }
    //     };
    //     log::info!("Starting mass erase...");
    //     let mut erase_reg = ERASEALL::from(1);
    //     let status_reg = ERASEALLSTATUS::from(0);
    //     let mut reset_reg = RESET::from(1);

    //     // Reset first
    //     self.write_ap_register(ctrl_port, reset_reg)?;
    //     reset_reg.RESET = false;
    //     self.write_ap_register(ctrl_port, reset_reg)?;

    //     self.write_ap_register(ctrl_port, erase_reg)?;

    //     // Prepare timeout
    //     let now = Instant::now();
    //     let status = self.read_ap_register(ctrl_port, status_reg)?;
    //     log::info!("Erase status: {:?}", status.ERASEALLSTATUS);
    //     let timeout = loop {
    //         let status = self.read_ap_register(ctrl_port, status_reg)?;
    //         if !status.ERASEALLSTATUS {
    //             break false;
    //         }
    //         if now.elapsed().as_secs() >= UNLOCK_TIMEOUT {
    //             break true;
    //         }
    //     };
    //     reset_reg.RESET = true;
    //     self.write_ap_register(ctrl_port, reset_reg)?;
    //     reset_reg.RESET = false;
    //     self.write_ap_register(ctrl_port, reset_reg)?;
    //     erase_reg.ERASEALL = false;
    //     self.write_ap_register(ctrl_port, erase_reg)?;
    //     if timeout {
    //         log::error!(
    //             "    {} Mass erase process timeout, the chip might still be locked.",
    //             "Error".red().bold()
    //         );
    //     } else {
    //         log::info!("Mass erase completed, chip unlocked");
    //     }
    //     Ok(())
    // }

    /// Get human readable name for the probe
    pub fn get_name(&self) -> String {
        self.inner.get_name().to_string()
    }

    /// Enters debug mode
    pub fn attach(mut self, target: impl Into<TargetSelector>) -> Result<Session, Error> {
        self.inner.attach()?;
        self.attached = true;

        Session::new(self, target)
    }

    pub fn attach_to_unspecified(&mut self) -> Result<(), Error> {
        self.inner.attach()?;
        self.attached = true;
        Ok(())
    }

    /// Selects the transport protocol to be used by the debug probe.
    pub fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        if !self.attached {
            self.inner.select_protocol(protocol)
        } else {
            Err(DebugProbeError::Attached)
        }
    }

    /// Leave debug mode
    pub fn detach(&mut self) -> Result<(), DebugProbeError> {
        self.attached = false;
        self.inner.detach()?;
        Ok(())
    }

    /// Resets the target device.
    pub fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.inner.target_reset()
    }

    /// Configure protocol speed to use in kHz
    pub fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        if !self.attached {
            self.inner.set_speed(speed_khz)
        } else {
            Err(DebugProbeError::Attached)
        }
    }

    /// Configured protocol speed in kHz
    pub fn speed_khz(&self) -> u32 {
        self.inner.speed()
    }

    /// Returns a probe specific memory interface if any is present for given probe.
    pub fn dedicated_memory_interface(&self) -> Result<Option<Memory>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.dedicated_memory_interface())
        }
    }

    pub fn has_dap_interface(&self) -> bool {
        self.inner.get_interface_dap().is_some()
    }

    pub fn get_interface_dap(&self) -> Result<Option<&dyn DAPAccess>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.get_interface_dap())
        }
    }

    pub fn get_interface_dap_mut(&mut self) -> Result<Option<&mut dyn DAPAccess>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.get_interface_dap_mut())
        }
    }

    pub fn has_jtag_interface(&self) -> bool {
        self.inner.get_interface_jtag().is_some()
    }

    pub fn get_interface_jtag(&self) -> Result<Option<&dyn JTAGAccess>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.get_interface_jtag())
        }
    }

    pub fn get_interface_jtag_mut(
        &mut self,
    ) -> Result<Option<&mut dyn JTAGAccess>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.get_interface_jtag_mut())
        }
    }
}

pub trait DebugProbe: Send + Sync + fmt::Debug {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized;

    /// Get human readable name for the probe
    fn get_name(&self) -> &str;

    /// Get the currently used maximum speed for the debug protocol in kHz.
    ///
    /// Not all probes report which speed is used, meaning this value is not
    /// always the actual speed used. However, the speed should not be any
    /// higher than this value.
    fn speed(&self) -> u32;

    /// Set the speed in kHz used for communication with the target device.
    ///
    /// The desired speed might not be supported by the probe. If the desired
    /// speed is not directly supported, a lower speed will be selected if possible.
    ///
    /// If possible, the actual speed used is returned by the function. Some probes
    /// cannot report this, so the value may be inaccurate.
    ///
    /// If the requested speed is not supported,
    /// `DebugProbeError::UnsupportedSpeed` will be returned.
    ///
    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError>;

    /// Enters debug mode
    fn attach(&mut self) -> Result<(), DebugProbeError>;

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError>;

    /// Hard-resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;

    /// Selects the transport protocol to be used by the debug probe.
    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError>;

    /// Returns a probe specific memory interface if any is present for given probe.
    fn dedicated_memory_interface(&self) -> Option<Memory>;

    fn get_interface_dap(&self) -> Option<&dyn DAPAccess>;

    fn get_interface_dap_mut(&mut self) -> Option<&mut dyn DAPAccess>;

    fn get_interface_jtag(&self) -> Option<&dyn JTAGAccess>;

    fn get_interface_jtag_mut(&mut self) -> Option<&mut dyn JTAGAccess>;
}

#[derive(Debug, Clone, PartialEq)]
pub enum DebugProbeType {
    DAPLink,
    STLink,
    JLink,
}

#[derive(Clone)]
pub struct DebugProbeInfo {
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub probe_type: DebugProbeType,
}

impl std::fmt::Debug for DebugProbeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} (VID: {:04x}, PID: {:04x}, {} {:?})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number
                .clone()
                .map_or("".to_owned(), |v| format!("Serial: {},", v)),
            self.probe_type
        )
    }
}

impl DebugProbeInfo {
    /// Creates a new info struct that uniquely identifies a probe.
    pub fn new<S: Into<String>>(
        identifier: S,
        vendor_id: u16,
        product_id: u16,
        serial_number: Option<String>,
        probe_type: DebugProbeType,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            vendor_id,
            product_id,
            serial_number,
            probe_type,
        }
    }

    /// Open the probe described by this `DebugProbeInfo`.
    pub fn open(&self) -> Result<Probe, DebugProbeError> {
        Probe::open(self)
    }
}

#[derive(Error, Debug)]
pub enum DebugProbeSelectorParseError {
    #[error("The VID or PID could not be parsed: {0}")]
    ParseInt(#[from] std::num::ParseIntError),
    #[error("Please use a string in the form `VID:PID:<Serial>` where Serial is optional.")]
    Format,
}

/// A struct to describe the way a probe should be selected.
///
/// Construct this from a set of info or from a string.
///
/// Example:
/// ```
/// use std::convert::TryInto;
/// let selector: probe_rs::DebugProbeSelector = "1337:1337:SERIAL".try_into().unwrap();
/// ```
#[derive(Debug, Clone)]
pub struct DebugProbeSelector {
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
}

impl TryFrom<&str> for DebugProbeSelector {
    type Error = DebugProbeSelectorParseError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let split = value.split(':').collect::<Vec<_>>();
        let mut selector = if split.len() > 1 {
            DebugProbeSelector {
                vendor_id: u16::from_str_radix(split[0], 16)?,
                product_id: u16::from_str_radix(split[1], 16)?,
                serial_number: None,
            }
        } else {
            return Err(DebugProbeSelectorParseError::Format);
        };

        if split.len() == 3 {
            selector.serial_number = Some(split[2].to_string());
        }

        Ok(selector)
    }
}

impl TryFrom<String> for DebugProbeSelector {
    type Error = DebugProbeSelectorParseError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        TryFrom::<&str>::try_from(&value)
    }
}

impl std::str::FromStr for DebugProbeSelector {
    type Err = DebugProbeSelectorParseError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::try_from(s)
    }
}

impl From<DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: DebugProbeInfo) -> Self {
        DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number,
        }
    }
}

impl From<&DebugProbeInfo> for DebugProbeSelector {
    fn from(selector: &DebugProbeInfo) -> Self {
        DebugProbeSelector {
            vendor_id: selector.vendor_id,
            product_id: selector.product_id,
            serial_number: selector.serial_number.clone(),
        }
    }
}

#[derive(Default, Debug)]
pub struct FakeProbe;

impl DebugProbe for FakeProbe {
    fn new_from_selector(
        _selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::Other("This is a fake probe."),
        ))
    }

    /// Get human readable name for the probe
    fn get_name(&self) -> &str {
        "Mock probe for testing"
    }

    fn speed(&self) -> u32 {
        unimplemented!()
    }

    fn set_speed(&mut self, _speed_khz: u32) -> Result<u32, DebugProbeError> {
        unimplemented!()
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn select_protocol(&mut self, _protocol: WireProtocol) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    /// Leave debug mode
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    /// Resets the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe)
    }

    fn dedicated_memory_interface(&self) -> Option<Memory> {
        None
    }

    fn get_interface_dap(&self) -> Option<&dyn DAPAccess> {
        None
    }

    fn get_interface_dap_mut(&mut self) -> Option<&mut dyn DAPAccess> {
        None
    }

    fn get_interface_jtag(&self) -> Option<&dyn JTAGAccess> {
        None
    }

    fn get_interface_jtag_mut(&mut self) -> Option<&mut dyn JTAGAccess> {
        None
    }
}

impl DAPAccess for FakeProbe {
    /// Reads the DAP register on the specified port and address
    fn read_register(&mut self, _port: PortType, _addr: u16) -> Result<u32, DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe)
    }

    /// Writes a value to the DAP register on the specified port and address
    fn write_register(
        &mut self,
        _port: PortType,
        _addr: u16,
        _value: u32,
    ) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe)
    }
}

/// Low-Level Access to the JTAG protocol
///
/// This trait should be implemented by all probes which offer low-level access to
/// the JTAG protocol, i.e. directo control over the bytes sent and received.
pub trait JTAGAccess {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError>;

    /// For Riscv, and possibly other interfaces, the JTAG interface has to remain in
    /// the idle state for several cycles between consecutive accesses to the DR register.
    ///
    /// This function configures the number of idle cycles which are inserted after each access.
    fn set_idle_cycles(&mut self, idle_cycles: u8);

    /// Write to a JTAG register
    ///
    /// This function will perform a write to the IR register, if necessary,
    /// to select the correct register, and then to the DR register, to transmit the
    /// data. The data shifted out of the DR register will be returned.
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError>;
}
