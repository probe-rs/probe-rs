//! # Debugging toolset for embedded devices
//!
//!  
//! # Prerequisites
//!
//! - Udev rules
//!
//! # Examples
//!
//!
//! ## Halting the attached chip
//! ```no_run
//! # use probe_rs::Error;
//! use probe_rs::{Lister, Probe, Permissions};
//!
//! // Get a list of all available debug probes.
//! let lister = Lister::new();
//!
//! let probes = lister.list_all();
//!
//! // Use the first probe found.
//! let mut probe = probes[0].open(&lister)?;
//!
//! // Attach to a chip.
//! let mut session = probe.attach("nrf52", Permissions::default())?;
//!
//! // Select a core.
//! let mut core = session.core(0)?;
//!
//! // Halt the attached core.
//! core.halt(std::time::Duration::from_millis(10))?;
//! # Ok::<(), Error>(())
//! ```
//!
//! ## Reading from RAM
//!
//! ```no_run
//! # use probe_rs::Error;
//! use probe_rs::{Session, Permissions, MemoryInterface};
//!
//! let mut session = Session::auto_attach("nrf52", Permissions::default())?;
//! let mut core = session.core(0)?;
//!
//! // Read a block of 50 32 bit words.
//! let mut buff = [0u32;50];
//! core.read_32(0x2000_0000, &mut buff)?;
//!
//! // Read a single 32 bit word.
//! let word = core.read_word_32(0x2000_0000)?;
//!
//! // Writing is just as simple.
//! let buff = [0u32;50];
//! core.write_32(0x2000_0000, &buff)?;
//!
//! // of course we can also write 8bit words.
//! let buff = [0u8;50];
//! core.write_8(0x2000_0000, &buff)?;
//!
//! # Ok::<(), Error>(())
//! ```
//!
//! probe-rs is built around 4 main interfaces: the [Probe],
//! [Target], [Session]  and [Core] structs.

#![recursion_limit = "256"]

#[macro_use]
extern crate serde;

/// All the interface bits for the different architectures.
pub mod architecture;
pub mod config;

#[warn(missing_docs)]
mod core;
pub mod debug;
mod error;
#[warn(missing_docs)]
pub mod flashing;
#[cfg(feature = "gdb-server")]
pub mod gdb_server;
pub mod integration;
#[warn(missing_docs)]
mod memory;
#[warn(missing_docs)]
mod probe;
#[warn(missing_docs)]
#[cfg(feature = "rtt")]
pub mod rtt;
#[warn(missing_docs)]
mod session;
#[cfg(test)]
mod test;

use probe_rs_target::ScanChainElement;

use crate::architecture::arm::communication_interface::{DapProbe, UninitializedArmProbe};
use crate::architecture::arm::sequences::{ArmDebugSequence, DefaultArmSequence};
use crate::architecture::arm::{ArmError, SwoAccess};
use crate::architecture::riscv::communication_interface::{
    RiscvCommunicationInterface, RiscvError,
};
use crate::architecture::xtensa::communication_interface::{
    XtensaCommunicationInterface, XtensaError,
};
use crate::config::TargetSelector;
pub use crate::config::{CoreType, InstructionSet, Target};
pub use crate::core::{
    exception_handler_for_core, Architecture, BreakpointCause, Core, CoreDump, CoreDumpError,
    CoreInformation, CoreInterface, CoreRegister, CoreRegisters, CoreState, CoreStatus, HaltReason,
    MemoryMappedRegister, RegisterId, RegisterRole, RegisterValue, SemihostingCommand,
    SpecificCoreState, VectorCatchCondition,
};
pub use crate::error::Error;
pub use crate::memory::MemoryInterface;
pub use crate::probe::{
    list::Lister, AttachMethod, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
    ProbeCreationError, ProbeFactory, WireProtocol,
};
pub use crate::session::{Permissions, Session};

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
/// use probe_rs::{Lister, Probe};
///
/// let lister = Lister::new();
///
/// let probe_list = lister.list_all();
/// let probe = probe_list[0].open(&lister);
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
    pub fn try_into_xtensa_interface(
        self,
    ) -> Result<XtensaCommunicationInterface, (Self, DebugProbeError)> {
        if !self.attached {
            Err((self, DebugProbeError::NotAttached))
        } else {
            self.inner
                .try_get_xtensa_interface()
                .map_err(|(probe, err)| (Probe::from_attached_probe(probe), err))
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

    /// Try to get a [`RiscvCommunicationInterface`], which can
    /// can be used to communicate with chips using the RISC-V
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

    /// Try reading the target voltage of via the connected voltage pin.
    ///
    /// This does not work on all probes.
    pub fn get_target_voltage(&mut self) -> Result<Option<f32>, DebugProbeError> {
        self.inner.get_target_voltage()
    }
}
