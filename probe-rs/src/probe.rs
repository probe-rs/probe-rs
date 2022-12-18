pub(crate) mod cmsisdap;
pub(crate) mod espusbjtag;
pub(crate) mod fake_probe;
#[cfg(feature = "ftdi")]
pub(crate) mod ftdi;
pub(crate) mod jlink;
pub(crate) mod stlink;

use self::espusbjtag::list_espjtag_devices;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::error::Error;
use crate::Session;
use crate::{
    architecture::arm::communication_interface::UninitializedArmProbe,
    config::{RegistryError, TargetSelector},
};
use crate::{
    architecture::{
        arm::{
            communication_interface::DapProbe,
            sequences::{ArmDebugSequence, DefaultArmSequence},
            PortType, SwoAccess,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
    },
    Permissions,
};
use jlink::list_jlink_devices;
use std::{convert::TryFrom, fmt};

/// Used to log warnings when the measured target voltage is
/// lower than 1.4V, if at all measureable.
const LOW_TARGET_VOLTAGE_WARNING_THRESHOLD: f32 = 1.4;

/// The protocol that is to be used by the probe when communicating with the target.
///
/// For ARM select `Swd` and for RISC-V select `Jtag`.
#[derive(Copy, Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum WireProtocol {
    /// Serial Wire Debug is ARMs proprietary standard for communicating with ARM cores.
    /// You can find specifics in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
    Swd,
    /// JTAG is a standard which is supported by many chips independent of architecture.
    /// See [`Wikipedia`](https://en.wikipedia.org/wiki/JTAG) for more info.
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

/// This error occurs whenever the debug probe logic encounters an error while operating the relevant debug probe.
#[derive(thiserror::Error, Debug)]
pub enum DebugProbeError {
    /// Something with the USB communication went wrong.
    #[error("USB Communication Error")]
    Usb(#[source] Option<Box<dyn std::error::Error + Send + Sync>>),
    /// The firmware of the probe is outdated. This error is especially prominent with ST-Links.
    /// You can use their official updater utility to update your probe firmware.
    #[error("The firmware on the probe is outdated")]
    ProbeFirmwareOutdated,
    /// An error which is specific to the debug probe in use occurred.
    #[error("An error specific to a probe type occurred")]
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// The debug probe handle could not be created as specified.
    #[error("Probe could not be created")]
    ProbeCouldNotBeCreated(#[from] ProbeCreationError),
    /// The selected wire protocol is not supported with given probe.
    #[error("Probe does not support {0}")]
    UnsupportedProtocol(WireProtocol),
    /// The selected probe does not support the selected interface.
    /// This happens if a probe does not support certain functionality, such as:
    /// - ARM debugging
    /// - RISC-V debugging
    /// - SWO
    #[error("The connected probe does not support the interface '{0}'")]
    InterfaceNotAvailable(&'static str),
    /// Some interaction with the target registry failed.
    /// This happens when an invalid chip name is given for example.
    #[error("An error occurred while working with the registry")]
    Registry(#[from] RegistryError),
    /// The debug probe does not support the speed that was chosen.
    /// Try to alter the selected speed.
    #[error("The requested speed setting ({0} kHz) is not supported by the probe")]
    UnsupportedSpeed(u32),
    /// The debug probe did not yet perform the init sequence.
    /// Try calling [`DebugProbe::attach`] before trying again.
    #[error("You need to be attached to the target to perform this action")]
    NotAttached,
    /// The debug probe already performed the init sequence.
    /// Try runnoing the failing command before [`DebugProbe::attach`].
    #[error("You need to be detached from the target to perform this action")]
    Attached,
    /// Performing the init sequence on the target failed.
    /// Check the wiring before continuing.
    #[error("Failed to find the target or attach to the target")]
    TargetNotFound,
    /// The variant of the function you called is not yet implemented.
    /// This can happen if some debug probe has some unimplemented functionality for a specific protocol or architecture.
    #[error("Some functionality was not implemented yet: {0}")]
    NotImplemented(&'static str),
    /// The called debug sequence is not supported on given probe.
    /// This is most likely happening because you are using an ST-Link, which are severely limited in functionality.
    /// If possible, try using another probe.
    #[error("This debug sequence is not supported on the used probe: {0}")]
    DebugSequenceNotSupported(&'static str),
    /// An error occurred during the previously batched command.
    #[error("Error in previous batched command")]
    BatchError(BatchCommand),
    /// The used functionality is not supported by the selected probe.
    /// This can happen when a probe does not allow for setting speed manually for example.
    #[error("Command not supported by probe: {0}")]
    CommandNotSupportedByProbe(&'static str),
    /// Some other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),

    /// A timeout occured during probe operation.
    #[error("Timeout occured during probe operation.")]
    Timeout,
}

/// An error during probe creation accured.
/// This is almost always a sign of a bad USB setup.
/// Check UDEV rules if you are on Linux and try installing Zadig
/// (This will disable vendor specific drivers for your probe!) if you are on Windows.
#[derive(thiserror::Error, Debug)]
pub enum ProbeCreationError {
    /// The selected debug probe was not found.
    /// This can be due to permissions.
    #[error("Probe was not found.")]
    NotFound,
    /// The selected probe USB device could not be opened.
    /// Make sure you have all necessary permissions.
    #[error("USB device could not be opened. Please check the permissions.")]
    CouldNotOpen,
    /// Some error with HID API occurred.
    #[error("{0}")]
    HidApi(#[from] hidapi::HidError),
    /// Some error with rusb occurred.
    #[error("{0}")]
    Rusb(#[from] rusb::Error),
    /// An error specific with the selected probe occurred.
    #[error("An error specific to a probe type occurred: {0}")]
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Something else happened.
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
    /// Create a new probe from a more specific probe driver.
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

    /// Same as [`Probe::new`] but without automatic boxing in case you already have a box.
    pub fn from_specific_probe(probe: Box<dyn DebugProbe>) -> Self {
        Probe {
            inner: probe,
            attached: false,
        }
    }

    /// Get a list of all debug probes found.
    /// This can be used to select the debug probe which
    /// should be used.
    #[tracing::instrument]
    pub fn list_all() -> Vec<DebugProbeInfo> {
        let mut list = cmsisdap::tools::list_cmsisdap_devices();
        #[cfg(feature = "ftdi")]
        {
            list.extend(ftdi::list_ftdi_devices());
        }
        list.extend(stlink::tools::list_stlink_devices());

        list.extend(list_jlink_devices());

        list.extend(list_espjtag_devices());

        list
    }

    /// Create a [`Probe`] from [`DebugProbeInfo`]. Use the
    /// [`Probe::list_all()`] function to get the information
    /// about all probes available.
    #[tracing::instrument(skip_all)]
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
        match jlink::JLink::new_from_selector(selector.clone()) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };
        match espusbjtag::EspUsbJtag::new_from_selector(selector) {
            Ok(link) => return Ok(Probe::from_specific_probe(link)),
            Err(DebugProbeError::ProbeCouldNotBeCreated(ProbeCreationError::NotFound)) => {}
            Err(e) => return Err(e),
        };

        Err(DebugProbeError::ProbeCouldNotBeCreated(
            ProbeCreationError::NotFound,
        ))
    }

    /// Get the human readable name for the probe.
    pub fn get_name(&self) -> String {
        self.inner.get_name().to_string()
    }

    /// Attach to the chip.
    ///
    /// This runs all the necessary protocol init routines.
    ///
    /// If this doesn't work, you might want to try [`Probe::attach_under_reset`]
    pub fn attach(
        mut self,
        target: impl Into<TargetSelector>,
        permissions: Permissions,
    ) -> Result<Session, Error> {
        self.attached = true;

        Session::new(self, target.into(), AttachMethod::Normal, permissions)
    }

    /// Attach to a target without knowing what target you have at hand.
    /// This can be used for automatic device discovery or performing operations on an unspecified target.
    pub fn attach_to_unspecified(&mut self) -> Result<(), Error> {
        self.inner.attach()?;
        self.attached = true;
        Ok(())
    }

    /// A combination of [`Probe::attach_to_unspecified`] and [`Probe::attach_under_reset`].
    pub fn attach_to_unspecified_under_reset(&mut self) -> Result<(), Error> {
        if let Some(dap_probe) = self.try_as_dap_probe() {
            DefaultArmSequence(()).reset_hardware_assert(dap_probe)?;
        } else {
            tracing::info!(
                "Custom reset sequences are not supported on {}.",
                self.get_name()
            );
            tracing::info!("Falling back to standard probe reset.");
            self.target_reset_assert()?;
        }

        self.inner_attach()?;
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
        permissions: Permissions,
    ) -> Result<Session, Error> {
        self.attached = true;
        // The session will de-assert reset after connecting to the debug interface.
        Session::new(self, target.into(), AttachMethod::UnderReset, permissions).map_err(|e| {
            if matches!(e, Error::Timeout) {
                Error::Other(
                anyhow::anyhow!("Timeout while attaching to target under reset. This can happen if the target is not responding to the reset sequence. Ensure the chip's reset pin is connected, or try attaching without reset."))
            } else {
                e
            }
        })
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

    /// Get the currently selected protocol
    ///
    /// Depending on the probe, this might not be available.
    pub fn protocol(&self) -> Option<WireProtocol> {
        self.inner.active_protocol()
    }

    /// Leave debug mode
    pub fn detach(&mut self) -> Result<(), crate::Error> {
        self.attached = false;
        self.inner.detach()?;
        Ok(())
    }

    /// Resets the target device.
    pub fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.inner.target_reset()
    }

    /// Asserts the reset of the target.
    /// This is always the hard reset which means the reset wire has to be connected to work.
    ///
    /// This is not supported on all probes.
    pub fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Asserting target reset");
        self.inner.target_reset_assert()
    }

    /// Deasserts the reset of the target.
    /// This is always the hard reset which means the reset wire has to be connected to work.
    ///
    /// This is not supported on all probes.
    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Deasserting target reset");
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

    /// Get the currently used maximum speed for the debug protocol in kHz.
    ///
    /// Not all probes report which speed is used, meaning this value is not
    /// always the actual speed used. However, the speed should not be any
    /// higher than this value.
    pub fn speed_khz(&self) -> u32 {
        self.inner.speed_khz()
    }

    /// Check if the probe has an interface to
    /// debug ARM chips.
    pub fn has_arm_interface(&self) -> bool {
        self.inner.has_arm_interface()
    }

    /// Try to get a trait object implementing `UninitializedArmProbe`, which can
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
    ) -> Result<RiscvCommunicationInterface, (Self, RiscvError)> {
        if !self.attached {
            Err((self, DebugProbeError::NotAttached.into()))
        } else {
            self.inner
                .try_get_riscv_interface()
                .map_err(|(probe, err)| (Probe::from_attached_probe(probe), err))
        }
    }

    /// Gets a SWO interface from the debug probe.
    ///
    /// This does not work on all probes.
    pub fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        self.inner.get_swo_interface()
    }

    /// Gets a mutable SWO interface from the debug probe.
    ///
    /// This does not work on all probes.
    pub fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        self.inner.get_swo_interface_mut()
    }

    /// Gets a DAP interface from the debug probe.
    ///
    /// This does not work on all probes.
    pub fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        self.inner.try_as_dap_probe()
    }

    /// Try reading the target voltage of via the connected volgate pin.
    ///
    /// This does not work on all probes.
    pub fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        self.inner.get_target_voltage()
    }
}

/// An abstraction over general debug probe functionality.
///
/// This trait has to be implemented by ever debug probe driver.
pub trait DebugProbe: Send + fmt::Debug {
    /// Creates a new boxed [`DebugProbe`] from a given [`DebugProbeSelector`].
    /// This will be called for all available debug drivers when discovering probes.
    /// When opening, it will open the first probe which succeds during this call.
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized;

    /// Get human readable name for the probe.
    fn get_name(&self) -> &str;

    /// Get the currently used maximum speed for the debug protocol in kHz.
    ///
    /// Not all probes report which speed is used, meaning this value is not
    /// always the actual speed used. However, the speed should not be any
    /// higher than this value.
    fn speed_khz(&self) -> u32;

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
    ///
    /// If the probe uses batched commands, this will also cause all
    /// remaining commands to be executed. If an error occurs during
    /// this execution, the probe might remain in the attached state.
    fn detach(&mut self) -> Result<(), crate::Error>;

    /// This should hard reset the target device.
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;

    /// This should assert the reset pin of the target via debug probe.
    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError>;

    /// This should deassert the reset pin of the target via debug probe.
    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError>;

    /// Selects the transport protocol to be used by the debug probe.
    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError>;

    /// Get the transport protocol currently in active use by the debug probe.
    fn active_protocol(&self) -> Option<WireProtocol>;

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
    ) -> Result<RiscvCommunicationInterface, (Box<dyn DebugProbe>, RiscvError)> {
        Err((
            self.into_probe(),
            DebugProbeError::InterfaceNotAvailable("RISCV").into(),
        ))
    }

    /// Check if the probe offers an interface to debug RISCV chips.
    fn has_riscv_interface(&self) -> bool {
        false
    }

    /// Get a SWO interface from the debug probe.
    ///
    /// This is not available on all debug probes.
    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        None
    }

    /// Get a mutable SWO interface from the debug probe.
    ///
    /// This is not available on all debug probes.
    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        None
    }

    /// Boxes itself.
    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe>;

    /// Try creating a DAP interface for the given probe.
    ///
    /// This is not available on all probes.
    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        None
    }

    /// Reads the target voltage in Volts, if possible. Returns `Ok(None)`
    /// if the probe doesn’t support reading the target voltage.
    fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        Ok(None)
    }
}

/// Denotes the type of a given [`DebugProbe`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugProbeType {
    /// CMSIS-DAP
    CmsisDap,
    /// FTDI based debug probe
    Ftdi,
    /// ST-Link
    StLink,
    /// J-Link
    JLink,
    /// Built in RISC-V ESP JTAG debug probe
    EspJtag,
}

/// Gathers some information about a debug probe which was found during a scan.
#[derive(Clone, PartialEq, Eq)]
pub struct DebugProbeInfo {
    /// The name of the debug probe.
    pub identifier: String,
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The serial number of the debug probe.
    pub serial_number: Option<String>,
    /// The probe type of the debug probe.
    pub probe_type: DebugProbeType,

    /// The USB HID interface which should be used.
    /// This is necessary for composite HID devices.
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
// We need this so that serde will first convert from the string `PID:VID:<Serial>` to a struct before deserializing.
#[serde(try_from = "String")]
pub struct DebugProbeSelector {
    /// The the USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The the USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The the serial number of the debug probe to be used.
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

    /// Return the currently configured idle cycles.
    fn get_idle_cycles(&self) -> u8;

    /// Set the IR register length
    fn set_ir_len(&mut self, len: u32);

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

    fn write_register_batch(
        &mut self,
        writes: &[JtagWriteCommand],
    ) -> Result<Vec<CommandResult>, BatchExecutionError> {
        let mut results = Vec::new();

        for write in writes {
            match self
                .write_register(write.address, &write.data, write.len)
                .map_err(crate::Error::Probe)
                .and_then(|response| (write.transform)(response))
            {
                Ok(res) => results.push(res),
                Err(e) => return Err(BatchExecutionError::new(e, results.clone())),
            }
        }

        Ok(results)
    }
}

pub type DeferredResultIndex = usize;

#[derive(Debug, Clone)]
pub struct JtagWriteCommand {
    pub address: u32,
    pub data: Vec<u8>,
    pub len: u32,
    pub transform: fn(Vec<u8>) -> Result<CommandResult, crate::Error>,
}

#[derive(thiserror::Error, Debug)]
pub struct BatchExecutionError {
    #[source]
    pub error: crate::Error,
    pub results: Vec<CommandResult>,
}

impl BatchExecutionError {
    pub fn new(error: crate::Error, results: Vec<CommandResult>) -> BatchExecutionError {
        BatchExecutionError { error, results }
    }
}

impl std::fmt::Display for BatchExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Error cause was {}. Successful command count {}",
            self.error,
            self.results.len()
        )
    }
}

/// Results generated by `JtagCommand`s
#[derive(Debug, Clone)]
pub enum CommandResult {
    None,
    U8(u8),
    U16(u16),
    U32(u32),
    VecU8(Vec<u8>),
}

/// The method that should be used for attaching.
#[derive(PartialEq, Eq, Debug, Copy, Clone)]
pub enum AttachMethod {
    /// Attach normally with no special behavior.
    Normal,
    /// Attach to the target while it is in reset.
    ///
    /// This is required on targets that can remap SWD pins or disable the SWD interface in sleep.
    UnderReset,
}
