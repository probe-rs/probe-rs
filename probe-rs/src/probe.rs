//! Probe drivers
pub(crate) mod arm_debug_interface;
pub(crate) mod common;
pub(crate) mod usb_util;

pub mod cmsisdap;
pub mod espusbjtag;
pub mod fake_probe;
pub mod ftdi;
pub mod jlink;
pub mod list;
pub mod stlink;
pub mod wlink;

use crate::architecture::arm::sequences::{ArmDebugSequence, DefaultArmSequence};
use crate::architecture::arm::ArmError;
use crate::architecture::arm::{
    communication_interface::{DapProbe, UninitializedArmProbe},
    PortType, SwoAccess,
};
use crate::architecture::riscv::communication_interface::{RiscvError, RiscvFactory};
use crate::architecture::xtensa::communication_interface::{
    XtensaCommunicationInterface, XtensaError,
};
use crate::config::RegistryError;
use crate::config::TargetSelector;
use crate::probe::common::IdCode;
use crate::{Error, Permissions, Session};
use nusb::DeviceInfo;
use probe_rs_target::ScanChainElement;
use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// Used to log warnings when the measured target voltage is
/// lower than 1.4V, if at all measurable.
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
                "'{s}' is not a valid protocol. Choose from [swd, jtag]."
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
    /// Read from a port
    Read(PortType, u16),

    /// Write to a port
    Write(PortType, u16, u32),
}

impl fmt::Display for BatchCommand {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            BatchCommand::Read(port, addr) => {
                write!(f, "Read(port={port:?}, addr={addr})")
            }
            BatchCommand::Write(port, addr, data) => {
                write!(f, "Write(port={port:?}, addr={addr}, data=0x{data:08x}")
            }
        }
    }
}

/// This error occurs whenever the debug probe logic encounters an error while operating the relevant debug probe.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum DebugProbeError {
    /// USB Communication Error
    Usb(#[source] std::io::Error),

    /// The firmware on the probe is outdated, and not supported by probe-rs.
    ///
    /// This error is especially prominent with ST-Links.
    /// You can use their official updater utility to update your probe firmware.
    // TODO: Shouldn't this be probe-specific?
    #[ignore_extra_doc_attributes]
    ProbeFirmwareOutdated,

    /// An error which is specific to the debug probe in use occurred.
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),

    /// The debug probe could not be created.
    ProbeCouldNotBeCreated(#[from] ProbeCreationError),

    /// The probe does not support the {0} protocol.
    UnsupportedProtocol(WireProtocol),

    /// The selected probe does not support the '{interface_name}' interface.
    ///
    /// This happens if a probe does not support certain functionality, such as:
    /// - ARM debugging
    /// - RISC-V debugging
    /// - SWO
    #[ignore_extra_doc_attributes]
    InterfaceNotAvailable {
        /// The name of the unsupported interface.
        interface_name: &'static str,
    },

    /// An error occurred while working with the registry.
    Registry(#[from] RegistryError),

    /// The probe does not support he requested speed setting ({0} kHz).
    UnsupportedSpeed(u32),

    /// You need to be attached to the target to perform this action.
    ///
    /// The debug probe did not yet perform the init sequence.
    /// Try calling [`DebugProbe::attach`] before trying again.
    #[ignore_extra_doc_attributes]
    NotAttached,

    /// You need to be detached from the target to perform this action.
    ///
    /// The debug probe already performed the init sequence.
    /// Try running the failing command before [`DebugProbe::attach`].
    #[ignore_extra_doc_attributes]
    Attached,

    /// Failed to find or attach to the target. Please check the wiring before retrying.
    TargetNotFound,

    /// Error in previous batched command.
    BatchError(BatchCommand),

    /// The '{function_name}' functionality is not implemented yet.
    ///
    /// The variant of the function you called is not yet implemented.
    /// This can happen if some debug probe has some unimplemented functionality for a specific protocol or architecture.
    #[ignore_extra_doc_attributes]
    NotImplemented {
        /// The name of the unsupported functionality.
        function_name: &'static str,
    },

    /// The '{command_name}' functionality is not supported by the selected probe.
    /// This can happen when a probe does not allow for setting speed manually for example.
    CommandNotSupportedByProbe {
        /// The name of the unsupported command.
        command_name: &'static str,
    },

    /// Some other error occurred.
    #[display("{0}")]
    Other(#[from] anyhow::Error),

    /// A timeout occurred during probe operation.
    Timeout,
}

/// An error during probe creation occurred.
/// This is almost always a sign of a bad USB setup.
/// Check UDEV rules if you are on Linux and try installing Zadig
/// (This will disable vendor specific drivers for your probe!) if you are on Windows.
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum ProbeCreationError {
    /// The selected debug probe was not found.
    /// This can be due to permissions.
    NotFound,
    /// The selected USB device could not be opened.
    CouldNotOpen,
    /// An HID API occurred.
    HidApi(#[from] hidapi::HidError),
    /// A USB error occurred.
    Usb(#[source] std::io::Error),
    /// An error specific with the selected probe occurred.
    ProbeSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    /// Something else happened.
    #[display("{0}")]
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
/// use probe_rs::probe::{Probe, list::Lister};
///
/// let lister = Lister::new();
///
/// let probe_list = lister.list_all();
/// let probe = probe_list[0].open();
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
        self,
        target: impl Into<TargetSelector>,
        permissions: Permissions,
    ) -> Result<Session, Error> {
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
        self.attach_to_unspecified()?;
        Ok(())
    }

    /// Attach to the chip under hard-reset.
    ///
    /// This asserts the reset pin via the probe, plays the protocol init routines and deasserts the pin.
    /// This is necessary if the chip is not responding to the SWD reset sequence.
    /// For example this can happen if the chip has the SWDIO pin remapped.
    pub fn attach_under_reset(
        self,
        target: impl Into<TargetSelector>,
        permissions: Permissions,
    ) -> Result<Session, Error> {
        // The session will de-assert reset after connecting to the debug interface.
        Session::new(self, target.into(), AttachMethod::UnderReset, permissions).map_err(|e| {
            if matches!(e, Error::Arm(ArmError::Timeout) | Error::Riscv(RiscvError::Timeout)| Error::Xtensa(XtensaError::Timeout)) {
                Error::Other(
                anyhow::anyhow!("Timeout while attaching to target under reset. This can happen if the target is not responding to the reset sequence. Ensure the chip's reset pin is connected, or try attaching without reset (`connectUnderReset = false` for DAP Clients, or remove `connect-under-reset` option from CLI options.)."))
            } else {
                e
            }
        })
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

    /// Configure the scan chain to use for the attached target.
    ///
    /// See [`DebugProbe::set_scan_chain`] for more information and usage
    pub fn set_scan_chain(
        &mut self,
        scan_chain: Vec<ScanChainElement>,
    ) -> Result<(), DebugProbeError> {
        if !self.attached {
            self.inner.set_scan_chain(scan_chain)
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
    /// debug Xtensa chips.
    pub fn has_xtensa_interface(&self) -> bool {
        self.inner.has_xtensa_interface()
    }

    /// Try to get a [`XtensaCommunicationInterface`], which can
    /// can be used to communicate with chips using the Xtensa
    /// architecture.
    ///
    /// If an error occurs while trying to connect, the probe is returned.
    pub fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        state: &'probe mut dyn Any,
    ) -> Result<XtensaCommunicationInterface<'probe>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            Ok(self.inner.try_get_xtensa_interface(state)?)
        }
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
    /// debug RISC-V chips.
    pub fn has_riscv_interface(&self) -> bool {
        self.inner.has_riscv_interface()
    }

    /// Try to get a [`RiscvFactory`] object, which can be used to set up a communication interface
    /// with chips using the RISC-V architecture.
    ///
    /// If an error occurs while trying to connect, the probe is returned.
    pub fn try_get_riscv_interface_factory<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvFactory<'probe> + 'probe>, DebugProbeError> {
        if !self.attached {
            Err(DebugProbeError::NotAttached)
        } else {
            self.inner.try_get_riscv_interface_factory()
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

    /// Try reading the target voltage of via the connected voltage pin.
    ///
    /// This does not work on all probes.
    pub fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        self.inner.get_target_voltage()
    }

    /// Try to get a J-Link interface from the debug probe.
    pub fn try_into_jlink(&mut self) -> Result<&mut jlink::JLink, DebugProbeError> {
        self.inner.try_into_jlink()
    }
}

/// An abstraction over a probe driver type.
///
/// This trait has to be implemented by ever debug probe driver.
///
/// The `std::fmt::Display` implementation will be used to display the probe in the list of available probes,
/// and should return a human-readable name for the probe type.
pub trait ProbeFactory: std::any::Any + std::fmt::Display + std::fmt::Debug + Sync {
    /// Creates a new boxed [`DebugProbe`] from a given [`DebugProbeSelector`].
    /// This will be called for all available debug drivers when discovering probes.
    /// When opening, it will open the first probe which succeeds during this call.
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError>;

    /// Returns a list of all available debug probes of the current type.
    fn list_probes(&self) -> Vec<DebugProbeInfo>;
}

/// An abstraction over general debug probe.
///
/// This trait has to be implemented by ever debug probe driver.
pub trait DebugProbe: Send + fmt::Debug {
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

    /// Set the JTAG scan chain information for the target under debug.
    ///
    /// This allows the probe to know which TAPs are in the scan chain and their
    /// position and IR lengths.
    ///
    /// If the scan chain is provided, and the selected protocol is JTAG, the
    /// probe will automatically configure the JTAG interface to match the
    /// scan chain configuration without trying to determine the chain at
    /// runtime.
    ///
    /// This is called by the `Session` when attaching to a target.
    /// So this does not need to be called manually, unless you want to
    /// modify the scan chain. You must be attached to a target to set the
    /// scan_chain since the scan chain only applies to the attached target.
    ///
    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError>;

    /// Attach to the chip.
    ///
    /// This should run all the necessary protocol init routines.
    fn attach(&mut self) -> Result<(), DebugProbeError>;

    /// Selects the JTAG TAP to be used for communication.
    fn select_jtag_tap(&mut self, index: usize) -> Result<(), DebugProbeError> {
        if index != 0 {
            return Err(DebugProbeError::NotImplemented {
                function_name: "select_jtag_tap",
            });
        }

        Ok(())
    }

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

    /// Check if the probe offers an interface to debug ARM chips.
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
            DebugProbeError::InterfaceNotAvailable {
                interface_name: "ARM",
            },
        ))
    }

    /// Try to get a [`RiscvFactory`] object, which can be used to set up a communication interface
    /// with chips using the RISC-V architecture.
    ///
    /// Ensure that the probe actually supports this by calling
    /// [DebugProbe::has_riscv_interface] first.
    fn try_get_riscv_interface_factory<'probe>(
        &'probe mut self,
    ) -> Result<Box<dyn RiscvFactory<'probe> + 'probe>, DebugProbeError> {
        Err(DebugProbeError::InterfaceNotAvailable {
            interface_name: "RISC-V",
        })
    }

    /// Check if the probe offers an interface to debug RISC-V chips.
    fn has_riscv_interface(&self) -> bool {
        false
    }

    /// Get the dedicated interface to debug Xtensa chips. Ensure that the
    /// probe actually supports this by calling [DebugProbe::has_xtensa_interface] first.
    fn try_get_xtensa_interface<'probe>(
        &'probe mut self,
        _state: &'probe mut dyn Any,
    ) -> Result<XtensaCommunicationInterface<'probe>, DebugProbeError> {
        Err(DebugProbeError::InterfaceNotAvailable {
            interface_name: "Xtensa",
        })
    }

    /// Check if the probe offers an interface to debug Xtensa chips.
    fn has_xtensa_interface(&self) -> bool {
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

    /// Try to get a J-Link interface from the debug probe.
    fn try_into_jlink(&mut self) -> Result<&mut jlink::JLink, DebugProbeError> {
        Err(DebugProbeError::Other(anyhow::anyhow!(
            "This probe does not support J-Link functionality."
        )))
    }
}

impl PartialEq for dyn ProbeFactory {
    fn eq(&self, other: &Self) -> bool {
        // Consider ProbeFactory objects equal when their types and data pointers are equal.
        // Pointer equality is insufficient, because ZST objects may have the same dangling pointer
        // as their address.
        self.type_id() == other.type_id()
            && std::ptr::eq(
                self as *const _ as *const (),
                other as *const _ as *const (),
            )
    }
}

/// Gathers some information about a debug probe which was found during a scan.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugProbeInfo {
    /// The name of the debug probe.
    pub identifier: String,
    /// The USB vendor ID of the debug probe.
    pub vendor_id: u16,
    /// The USB product ID of the debug probe.
    pub product_id: u16,
    /// The serial number of the debug probe.
    pub serial_number: Option<String>,

    /// The USB HID interface which should be used.
    /// This is necessary for composite HID devices.
    pub hid_interface: Option<u8>,

    /// A reference to the [`ProbeFactory`] that created this info object.
    probe_factory: &'static dyn ProbeFactory,
}

impl std::fmt::Display for DebugProbeInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(
            f,
            "{} (VID: {:04x}, PID: {:04x}, {}{})",
            self.identifier,
            self.vendor_id,
            self.product_id,
            self.serial_number
                .as_ref()
                .map_or("".to_owned(), |v| format!("Serial: {v}, ")),
            self.probe_factory
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
        probe_factory: &'static dyn ProbeFactory,
        hid_interface: Option<u8>,
    ) -> Self {
        Self {
            identifier: identifier.into(),
            vendor_id,
            product_id,
            serial_number,
            probe_factory,
            hid_interface,
        }
    }

    /// Open the probe described by this `DebugProbeInfo`.
    pub fn open(&self) -> Result<Probe, DebugProbeError> {
        let selector = DebugProbeSelector::from(self);
        self.probe_factory
            .open(&selector)
            .map(Probe::from_specific_probe)
    }

    /// Returns whether this info was returned by a particular probe factory.
    pub fn is_probe_type<F: ProbeFactory>(&self) -> bool {
        self.probe_factory.type_id() == std::any::TypeId::of::<F>()
    }
}

/// An error which can occur while parsing a [`DebugProbeSelector`].
#[derive(thiserror::Error, Debug, docsplay::Display)]
pub enum DebugProbeSelectorParseError {
    /// Could not parse VID or PID: {0}
    ParseInt(#[from] std::num::ParseIntError),

    /// The format of the selector is invalid. Please use a string in the form `VID:PID:<Serial>`, where Serial is optional.
    Format,
}

/// A struct to describe the way a probe should be selected.
///
/// Construct this from a set of info or from a string. The
/// string has to be in the format "VID:PID:SERIALNUMBER",
/// where the serial number is optional, and VID and PID are
/// parsed as hexadecimal numbers.
///
/// ## Example:
///
/// ```
/// use std::convert::TryInto;
/// let selector: probe_rs::probe::DebugProbeSelector = "1942:1337:SERIAL".try_into().unwrap();
///
/// assert_eq!(selector.vendor_id, 0x1942);
/// assert_eq!(selector.product_id, 0x1337);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
// We need this so that serde will first convert from the string `VID:PID:<Serial>` to a struct before deserializing.
#[serde(try_from = "String")]
pub struct DebugProbeSelector {
    /// The the USB vendor id of the debug probe to be used.
    pub vendor_id: u16,
    /// The the USB product id of the debug probe to be used.
    pub product_id: u16,
    /// The the serial number of the debug probe to be used.
    pub serial_number: Option<String>,
}

impl DebugProbeSelector {
    pub(crate) fn matches(&self, info: &DeviceInfo) -> bool {
        info.vendor_id() == self.vendor_id
            && info.product_id() == self.product_id
            && self
                .serial_number
                .as_ref()
                .map(|s| info.serial_number() == Some(s))
                .unwrap_or(true)
    }
}

impl TryFrom<&str> for DebugProbeSelector {
    type Error = DebugProbeSelectorParseError;
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // Split into at most 3 parts: VID, PID, Serial.
        // We limit the number of splits to allow for colons in the
        // serial number (EspJtag uses MAC address)
        let mut split = value.splitn(3, ':');

        let vendor_id = split.next().unwrap(); // First split is always successful
        let product_id = split.next().ok_or(DebugProbeSelectorParseError::Format)?;
        let serial_number = split.next().map(|s| s.to_string());

        Ok(DebugProbeSelector {
            vendor_id: u16::from_str_radix(vendor_id, 16)?,
            product_id: u16::from_str_radix(product_id, 16)?,
            serial_number,
        })
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

impl From<&DebugProbeSelector> for DebugProbeSelector {
    fn from(selector: &DebugProbeSelector) -> Self {
        selector.clone()
    }
}

impl fmt::Display for DebugProbeSelector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04x}:{:04x}", self.vendor_id, self.product_id)?;
        if let Some(ref sn) = self.serial_number {
            write!(f, ":{sn}")?;
        }
        Ok(())
    }
}

/// Low-Level Access to the JTAG protocol
///
/// This trait should be implemented by all probes which offer low-level access to
/// the JTAG protocol, i.e. direction control over the bytes sent and received.
pub trait JTAGAccess: DebugProbe {
    /// Scans `IDCODE` and `IR` length information about the devices on the JTAG chain.
    ///
    /// If configured, this will use the data from [`DebugProbe::set_scan_chain`]. Otherwise, it
    /// will try to measure and extract `IR` lengths by driving the JTAG interface.
    ///
    /// The measured scan chain will be stored in the probe's internal state.
    fn scan_chain(&mut self) -> Result<(), DebugProbeError>;

    /// Executes a TAP reset.
    fn tap_reset(&mut self) -> Result<(), DebugProbeError>;

    /// Read a JTAG register.
    ///
    /// This function emulates a read by performing a write with all zeros to the DR.
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError> {
        let data = vec![0u8; (len as usize + 7) / 8];

        self.write_register(address, &data, len)
    }

    /// For RISC-V, and possibly other interfaces, the JTAG interface has to remain in
    /// the idle state for several cycles between consecutive accesses to the DR register.
    ///
    /// This function configures the number of idle cycles which are inserted after each access.
    fn set_idle_cycles(&mut self, idle_cycles: u8);

    /// Return the currently configured idle cycles.
    fn idle_cycles(&self) -> u8;

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

    /// Executes a sequence of JTAG commands.
    fn write_register_batch(
        &mut self,
        writes: &JtagCommandQueue,
    ) -> Result<DeferredResultSet, BatchExecutionError> {
        tracing::debug!("Using default `JTAGAccess::write_register_batch` this will hurt performance. Please implement proper batching for this probe.");
        let mut results = DeferredResultSet::new();

        for (idx, write) in writes.iter() {
            match self
                .write_register(write.address, &write.data, write.len)
                .map_err(crate::Error::Probe)
                .and_then(|response| (write.transform)(write, response))
            {
                Ok(res) => results.push(idx, res),
                Err(e) => return Err(BatchExecutionError::new(e, results)),
            }
        }

        Ok(results)
    }
}

/// A low-level JTAG register write command.
#[derive(Debug, Clone)]
pub struct JtagWriteCommand {
    /// The IR register to write to.
    pub address: u32,

    /// The data to be written to DR.
    pub data: Vec<u8>,

    /// The number of bits in `data`
    pub len: u32,

    /// A function to transform the raw response into a [`CommandResult`]
    pub transform: fn(&JtagWriteCommand, Vec<u8>) -> Result<CommandResult, crate::Error>,
}

/// Represents a Jtag Tap within the chain.
#[derive(Debug)]
pub struct JtagChainItem {
    /// The IDCODE of the device.
    pub idcode: Option<IdCode>,

    /// The length of the instruction register.
    pub irlen: usize,
}

/// Chain parameters to select a target tap within the chain.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChainParams {
    pub irpre: usize,
    pub irpost: usize,
    pub drpre: usize,
    pub drpost: usize,
    pub irlen: usize,
}

impl ChainParams {
    fn from_jtag_chain(chain: &[JtagChainItem], selected: usize) -> Option<Self> {
        let mut params = Self::default();

        let mut found = false;
        for (index, tap) in chain.iter().enumerate() {
            tracing::info!("{:?}", tap);
            if index == selected {
                params.irlen = tap.irlen;
                found = true;
            } else if found {
                params.irpost += tap.irlen;
                params.drpost += 1;
            } else {
                params.irpre += tap.irlen;
                params.drpre += 1;
            }
        }

        found.then_some(params)
    }
}

/// An error that occurred during batched command execution.
#[derive(thiserror::Error, Debug)]
pub struct BatchExecutionError {
    /// The error that occurred during execution.
    #[source]
    pub error: crate::Error,

    /// The results of the commands that were executed before the error occurred.
    pub results: DeferredResultSet,
}

impl BatchExecutionError {
    pub(crate) fn new(error: crate::Error, results: DeferredResultSet) -> BatchExecutionError {
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
    /// No result
    None,

    /// A single byte
    U8(u8),

    /// A single 16-bit word
    U16(u16),

    /// A single 32-bit word
    U32(u32),

    /// Multiple bytes
    VecU8(Vec<u8>),
}

impl CommandResult {
    /// Returns the result as a `u32` if possible.
    ///
    /// # Panics
    ///
    /// Panics if the result is not a `u32`.
    pub fn into_u32(self) -> u32 {
        match self {
            CommandResult::U32(val) => val,
            _ => panic!("CommandResult is not a u32"),
        }
    }

    /// Returns the result as a `u8` if possible.
    ///
    /// # Panics
    ///
    /// Panics if the result is not a `u8`.
    pub fn into_u8(self) -> u8 {
        match self {
            CommandResult::U8(val) => val,
            _ => panic!("CommandResult is not a u8"),
        }
    }
}

/// A set of batched commands that will be executed all at once.
///
/// This list maintains which commands' results can be read by the issuing code, which then
/// can be used to skip capturing or processing certain parts of the response.
#[derive(Default, Debug)]
pub struct JtagCommandQueue {
    commands: Vec<(DeferredResultIndex, JtagWriteCommand)>,
}

impl JtagCommandQueue {
    /// Creates a new empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedules a command for later execution.
    ///
    /// Returns a token value that can be used to retrieve the result of the command.
    pub fn schedule(&mut self, command: JtagWriteCommand) -> DeferredResultIndex {
        let index = DeferredResultIndex::new();
        self.commands.push((index.clone(), command));
        index
    }

    /// Returns the number of commands in the queue.
    pub fn len(&self) -> usize {
        self.commands.len()
    }

    /// Returns whether the queue is empty.
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &(DeferredResultIndex, JtagWriteCommand)> {
        self.commands.iter()
    }

    /// Removes the first `len` number of commands from the batch.
    pub(crate) fn consume(&mut self, len: usize) {
        self.commands.drain(..len);
    }
}

/// The set of results returned by executing a batched command.
#[derive(Debug, Default)]
pub struct DeferredResultSet(HashMap<DeferredResultIndex, CommandResult>);

impl DeferredResultSet {
    /// Creates a new empty result set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a new empty result set with the given capacity.
    pub fn with_capacity(capacity: usize) -> Self {
        Self(HashMap::with_capacity(capacity))
    }

    pub(crate) fn push(&mut self, idx: &DeferredResultIndex, result: CommandResult) {
        self.0.insert(idx.clone(), result);
    }

    /// Returns the number of results in the set.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn merge_from(&mut self, other: DeferredResultSet) {
        self.0.extend(other.0);
        self.0.retain(|k, _| k.should_capture());
    }

    /// Takes a result from the set.
    pub fn take(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, DeferredResultIndex> {
        self.0.remove(&index).ok_or(index)
    }
}

/// An index type used to retrieve the result of a deferred command.
///
/// This type can detect if the result of a command is not used.
#[derive(Eq)]
pub struct DeferredResultIndex(Arc<()>);

impl PartialEq for DeferredResultIndex {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl fmt::Debug for DeferredResultIndex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DeferredResultIndex")
            .field(&self.id())
            .finish()
    }
}

impl DeferredResultIndex {
    // Intentionally private. User code must not be able to create these.
    fn new() -> Self {
        Self(Arc::new(()))
    }

    fn id(&self) -> usize {
        Arc::as_ptr(&self.0) as usize
    }

    pub(crate) fn should_capture(&self) -> bool {
        // Both the queue and the user code may hold on to at most one of the references. The queue
        // execution will be able to detect if the user dropped their read reference, meaning
        // the read data would be inaccessible.
        Arc::strong_count(&self.0) > 1
    }

    // Intentionally private. User code must not be able to clone these.
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl std::hash::Hash for DeferredResultIndex {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id().hash(state)
    }
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

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_is_probe_factory() {
        let probe_info = DebugProbeInfo::new(
            "Mock probe",
            0x12,
            0x23,
            Some("mock_serial".to_owned()),
            &ftdi::FtdiProbeFactory,
            None,
        );

        assert!(probe_info.is_probe_type::<ftdi::FtdiProbeFactory>());
        assert!(!probe_info.is_probe_type::<espusbjtag::EspUsbJtagFactory>());
    }

    #[test]
    fn test_parsing_many_colons() {
        let selector: DebugProbeSelector = "303a:1001:DC:DA:0C:D3:FE:D8".try_into().unwrap();

        assert_eq!(selector.vendor_id, 0x303a);
        assert_eq!(selector.product_id, 0x1001);
        assert_eq!(
            selector.serial_number,
            Some("DC:DA:0C:D3:FE:D8".to_string())
        );
    }
}
