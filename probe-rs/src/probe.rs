pub(crate) mod cmsisdap;
pub(crate) mod fake_probe;
#[cfg(feature = "ftdi")]
pub(crate) mod ftdi;
pub(crate) mod jlink;
pub(crate) mod stlink;

use crate::architecture::{
    arm::{communication_interface::DapProbe, PortType, SwoAccess},
    riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::error::Error;
use crate::Session;
use crate::{
    architecture::arm::communication_interface::UninitializedArmProbe,
    config::{RegistryError, TargetSelector},
};
use jlink::list_jlink_devices;
use std::{convert::TryFrom, fmt};

/// Used to log warnings when the measured target voltage is
/// lower than 1.4V, if at all measureable.
const LOW_TARGET_VOLTAGE_WARNING_THRESHOLD: f32 = 1.4;

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

#[derive(thiserror::Error, Debug)]
pub enum DebugProbeError {
    #[error("USB Communication Error")]
    Usb(#[source] Option<Box<dyn std::error::Error + Send + Sync>>),
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
    #[error("Failed to find the target or attach to the target")]
    TargetNotFound,
    #[error("Some functionality was not implemented yet: {0}")]
    NotImplemented(&'static str),
    #[error("This debug sequence is not supported on the used probe: {0}")]
    DebugSequenceNotSupported(&'static str),
    #[error("Error in previous batched command")]
    BatchError(BatchCommand),
    #[error("Command not supported by probe: {0}")]
    CommandNotSupportedByProbe(&'static str),
    #[error("Unable to set hardware breakpoint, all available breakpoint units are in use.")]
    BreakpointUnitsExceeded,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

#[derive(thiserror::Error, Debug)]
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

    pub(crate) fn from_attached_probe(probe: Box<dyn DebugProbe>) -> Self {
        Self {
            inner: probe,
            attached: true,
        }
    }

    pub fn from_specific_probe(probe: Box<dyn DebugProbe>) -> Self {
        Probe {
            inner: probe,
            attached: false,
        }
    }

    /// Get a list of all debug probes found.
    /// This can be used to select the debug probe which
    /// should be used.
    pub fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = cmsisdap::tools::list_cmsisdap_devices();
        #[cfg(feature = "ftdi")]
        {
            list.extend(ftdi::list_ftdi_devices());
        }
        list.extend(stlink::tools::list_stlink_devices());

        list.extend(list_jlink_devices());

        list
    }

    /// Create a `Probe` from `DebugProbeInfo`. Use the
    /// `Probe::list_all()` function to get the information
    /// about all probes available.
    pub fn open(selector: impl Into<DebugProbeSelector> + Clone) -> Result<Self, DebugProbeError> {
        match cmsisdap::CmsisDap::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        #[cfg(feature = "ftdi")]
        match ftdi::FtdiProbe::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match stlink::StLink::new_from_selector(selector.clone()) {
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

    /// Get human readable name for the probe
    pub fn get_name(&self) -> String {
        self.inner.get_name().to_string()
    }

    /// Attach to the chip.
    ///
    /// This runs all the necessary protocol init routines.
    ///
    /// If this doesn't work, you might want to try `attach_under_reset`
    pub fn attach(mut self, target: impl Into<TargetSelector>) -> Result<Session, Error> {
        self.attached = true;

        Session::new(self, target.into(), AttachMethod::Normal)
    }

    pub fn attach_to_unspecified(&mut self) -> Result<(), Error> {
        self.inner.attach()?;
        self.attached = true;
        Ok(())
    }

    /// Attach to the chip under hard-reset.
    ///
    /// This asserts the reset pin via the probe, plays the protocol init routines and deasserts the pin.
    /// This is necessary if the chip is not responding to the SWD reset sequence.
    /// For example this can happen if the chip has the SWDIO pin remapped.
    pub fn attach_under_reset(
        mut self,
        target: impl Into<TargetSelector>,
    ) -> Result<Session, Error> {
        self.attached = true;

        // The session will de-assert reset after connecting to the debug interface.
        Session::new(self, target.into(), AttachMethod::UnderReset)
    }

    pub(crate) fn inner_attach(&mut self) -> Result<(), DebugProbeError> {
        self.inner.attach()
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

    pub(crate) fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Asserting target reset");
        self.inner.target_reset_assert()
    }

    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Deasserting target reset");
        self.inner.target_reset_deassert()
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

    /// Check if the probe has an interface to
    /// debug ARM chips.
    pub fn has_arm_interface(&self) -> bool {
        self.inner.has_arm_interface()
    }

    /// Try to get a trait object implementing [`ArmProbeInterface`], which can
    /// can be used to communicate with chips using the ARM architecture.
    ///
    /// If an error occurs while trying to connect, the probe is returned.
    pub fn try_into_arm_interface<'probe>(
        self,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Self, DebugProbeError)> {
        if !self.attached {
            Err((self, DebugProbeError::NotAttached))
        } else {
            self.inner
                .try_get_arm_interface()
                .map_err(|(probe, err)| (Probe::from_attached_probe(probe), err))
        }
    }

    /// Check if the probe has an interface to
    /// debug RISCV chips.
    pub fn has_riscv_interface(&self) -> bool {
        self.inner.has_riscv_interface()
    }

    /// Try to get a [`RiscvCommunicationInterface`], which can
    /// can be used to communicate with chips using the RISCV
    /// architecture.
    ///
    /// If an error occurs while trying to connect, the probe is returned.
    pub fn try_into_riscv_interface(
        self,
    ) -> Result<RiscvCommunicationInterface, (Self, DebugProbeError)> {
        if !self.attached {
            Err((self, DebugProbeError::NotAttached))
        } else {
            self.inner
                .try_get_riscv_interface()
                .map_err(|(probe, err)| (Probe::from_attached_probe(probe), err))
        }
    }

    pub fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        self.inner.get_swo_interface()
    }

    pub fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        self.inner.get_swo_interface_mut()
    }

    pub fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        self.inner.try_as_dap_probe()
    }
}

pub trait DebugProbe: Send + fmt::Debug {
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

    /// Attach to the chip.
    ///
    /// This should run all the necessary protocol init routines.
    fn attach(&mut self) -> Result<(), DebugProbeError>;

    /// Detach from the chip.
    ///
    /// This should run all the necessary protocol deinit routines.
    fn detach(&mut self) -> Result<(), DebugProbeError>;

    /// This should hard reset the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;

    /// This should assert the reset pin of the target via debug probe.
    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError>;

    /// This should deassert the reset pin of the target via debug probe.
    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError>;

    /// Selects the transport protocol to be used by the debug probe.
    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError>;

    /// Check if the proble offers an interface to debug ARM chips.
    fn has_arm_interface(&self) -> bool {
        false
    }

    /// Get the dedicated interface to debug ARM chips. To check that the
    /// probe actually supports this, call [DebugProbe::has_arm_interface] first.
    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        Err((
            self.into_probe(),
            DebugProbeError::InterfaceNotAvailable("ARM"),
        ))
    }

    /// Get the dedicated interface to debug RISCV chips. Ensure that the
    /// probe actually supports this by calling [DebugProbe::has_riscv_interface] first.
    fn try_get_riscv_interface(
        self: Box<Self>,
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        Err((
            self.into_probe(),
            DebugProbeError::InterfaceNotAvailable("RISCV"),
        ))
    }

    /// Check if the probe offers an interface to debug RISCV chips.
    fn has_riscv_interface(&self) -> bool {
        false
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        None
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        None
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe>;

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        None
    }

    /// Reads the target voltage in Volts, if possible. Returns `Ok(None)`
    /// if the probe doesn’t support reading the target voltage.
    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        Ok(None)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DebugProbeType {
    CmsisDap,
    Ftdi,
    StLink,
    JLink,
}

#[derive(Clone)]
pub struct DebugProbeInfo {
    pub identifier: String,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub probe_type: DebugProbeType,

    /// USB HID interface which should be used.
    /// Necessary for composite HID devices.
    pub hid_interface: Option<u8>,
}

impl std::fmt::Debug for DebugProbeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} (VID: {:04x}, PID: {:04x}, {}{:?})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number
                .clone()
                .map_or("".to_owned(), |v| format!("Serial: {}, ", v)),
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
        usb_hid_interface: Option<u8>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            vendor_id,
            product_id,
            serial_number,
            probe_type,
            hid_interface: usb_hid_interface,
        }
    }

    /// Open the probe described by this `DebugProbeInfo`.
    pub fn open(&self) -> Result<Probe, DebugProbeError> {
        Probe::open(self)
    }
}

#[derive(thiserror::Error, Debug)]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(try_from = "String")] //We need this so that serde will first converst from the string `PID:VID:<Serial>` to a struct before deserializing
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

impl fmt::Display for DebugProbeSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor_id, self.product_id)?;
        if let Some(ref sn) = self.serial_number {
            write!(f, ":{}", sn)?;
        }
        Ok(())
    }
}

/// Low-Level Access to the JTAG protocol
///
/// This trait should be implemented by all probes which offer low-level access to
/// the JTAG protocol, i.e. directo control over the bytes sent and received.
pub trait JTAGAccess: DebugProbe {
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

#[derive(PartialEq, Debug, Copy, Clone)]
pub enum AttachMethod {
    Normal,
    UnderReset,
}
