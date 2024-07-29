//! Vendor support modules.

use std::ops::Deref;

use once_cell::sync::Lazy;
use parking_lot::{RwLock, RwLockReadGuard};
use probe_rs_target::Chip;

use crate::{
    architecture::{
        arm::{sequences::DefaultArmSequence, ArmChipInfo, ArmProbeInterface, DpAddress},
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState,
        },
    },
    config::{registry, ChipInfo, DebugSequence},
    probe::{DebugProbeError, Probe},
    Error, Target,
};

pub mod espressif;
pub mod infineon;
pub mod microchip;
pub mod nordicsemi;
pub mod nxp;
pub mod silabs;
pub mod st;
pub mod ti;
pub mod vorago;

/// Vendor support trait.
pub trait Vendor: Send + Sync + std::fmt::Display {
    /// Tries to create a debug sequence for the given chip.
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence>;

    /// Tries to identify an ARM chip. Returns `Some(target name)` on success.
    fn try_detect_arm_chip(
        &self,
        _probe: &mut dyn ArmProbeInterface,
        _chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to identify an RISC-V chip. Returns `Some(target name)` on success.
    fn try_detect_riscv_chip(
        &self,
        _probe: &mut RiscvCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }

    /// Tries to identify an Xtensa chip. Returns `Some(target name)` on success.
    fn try_detect_xtensa_chip(
        &self,
        _probe: &mut XtensaCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }
}

static VENDORS: Lazy<RwLock<Vec<Box<dyn Vendor>>>> = Lazy::new(|| {
    let vendors: Vec<Box<dyn Vendor>> = vec![
        Box::new(microchip::Microchip),
        Box::new(infineon::Infineon),
        Box::new(silabs::SiliconLabs),
        Box::new(ti::TexasInstruments),
        Box::new(espressif::Espressif),
        Box::new(nordicsemi::NordicSemi),
        Box::new(nxp::Nxp),
        Box::new(st::St),
        Box::new(vorago::Vorago),
    ];

    RwLock::new(vendors)
});

/// Registers a new vendor.
pub fn register_vendor(vendor: Box<dyn Vendor>) {
    // Order matters. Prepend to allow users to override the default vendors.
    VENDORS.write().insert(0, vendor);
}

/// Returns a readable view of all known vendors.
fn vendors<'a>() -> impl Deref<Target = [Box<dyn Vendor>]> + 'a {
    RwLockReadGuard::map(VENDORS.read_recursive(), |v| v.as_slice())
}

/// Tries to create a debug sequence for the given chip.
pub fn try_create_debug_sequence(chip: &Chip) -> Option<DebugSequence> {
    let vendors = vendors();
    for vendor in vendors.iter() {
        if let Some(sequence) = vendor.try_create_debug_sequence(chip) {
            return Some(sequence);
        }
    }

    None
}

fn try_detect_arm_chip(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
    let mut found_target = None;

    if !probe.has_arm_interface() {
        // No ARM interface available.
        tracing::debug!("No ARM interface available, skipping detection.");
        return Ok((probe, None));
    }

    // We have no information about the target, so we must assume it's using the default DP.
    // We cannot automatically detect DPs if SWD multi-drop is used.
    // TODO: collect known DP addresses for known targets.
    let dp_addresses = [DpAddress::Default];

    for dp_address in dp_addresses {
        // TODO: do not consume probe
        match probe.try_into_arm_interface() {
            Ok(interface) => {
                let mut interface =
                    match interface.initialize(DefaultArmSequence::create(), dp_address) {
                        Ok(interface) => interface,
                        Err((interface, error)) => {
                            probe = interface.close();
                            tracing::debug!("Error during ARM chip detection: {error}");
                            // If we can't connect, assume this is not an ARM chip and not an error.
                            return Ok((probe, None));
                        }
                    };

                let found_arm_chip = interface
                    .read_chip_info_from_rom_table(dp_address)
                    .unwrap_or_else(|error| {
                        tracing::debug!("Error during ARM chip detection: {error}");
                        None
                    });

                if let Some(found_chip) = found_arm_chip {
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        // TODO: only consider families with matching JEP106.
                        if let Some(target_name) =
                            vendor.try_detect_arm_chip(interface.as_mut(), found_chip)?
                        {
                            found_target = Some(registry::get_target_by_name(&target_name)?);
                            break;
                        }
                    }

                    // No vendor-specific match, try to find a target by chip info.
                    if found_target.is_none() {
                        found_target = Some(crate::config::get_target_by_chip_info(
                            ChipInfo::from(found_chip),
                        )?);
                    }
                }

                probe = interface.close();
            }
            Err((returned_probe, error)) => {
                probe = returned_probe;
                tracing::debug!("Error using ARM interface: {error}");
            }
        }
    }

    Ok((probe, found_target))
}

fn try_detect_riscv_chip(probe: &mut Probe) -> Result<Option<Target>, Error> {
    let mut found_target = None;

    match probe.try_get_riscv_interface_builder() {
        Ok(factory) => {
            let mut state = factory.create_state();
            let mut interface = factory.attach(&mut state)?;

            if let Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter RISC-V debug mode: {error}");
                return Ok(None);
            }

            match interface.read_idcode() {
                Ok(Some(idcode)) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        // TODO: only consider families with matching JEP106.
                        if let Some(target_name) =
                            vendor.try_detect_riscv_chip(&mut interface, idcode)?
                        {
                            found_target = Some(registry::get_target_by_name(target_name)?);
                            break;
                        }
                    }
                }
                Ok(_) => tracing::debug!("No RISC-V ID code returned."),
                Err(error) => tracing::debug!("Error during RISC-V chip detection: {error}"),
            }

            // TODO: disable debug module
        }

        Err(DebugProbeError::InterfaceNotAvailable { .. }) => {
            tracing::debug!("No RISC-V interface available, skipping detection.");
        }

        Err(error) => {
            tracing::debug!("Error during RISC-V chip detection: {error}");
        }
    }

    Ok(found_target)
}

fn try_detect_xtensa_chip(probe: &mut Probe) -> Result<Option<Target>, Error> {
    let mut found_target = None;

    let mut state = XtensaDebugInterfaceState::default();
    match probe.try_get_xtensa_interface(&mut state) {
        Ok(mut interface) => {
            if let Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter Xtensa debug mode: {error}");
                return Ok(None);
            }

            match interface.read_idcode() {
                Ok(idcode) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        // TODO: only consider families with matching JEP106.
                        if let Some(target_name) =
                            vendor.try_detect_xtensa_chip(&mut interface, idcode)?
                        {
                            found_target = Some(registry::get_target_by_name(target_name)?);
                            break;
                        }
                    }
                }
                Err(error) => tracing::debug!("Error during Xtensa chip detection: {error}"),
            }

            interface.leave_debug_mode()?;
        }

        Err(DebugProbeError::InterfaceNotAvailable { .. }) => {
            tracing::debug!("No Xtensa interface available, skipping detection.");
        }

        Err(error) => {
            tracing::debug!("Error during autodetection of Xtensa chips: {error}");
        }
    }

    Ok(found_target)
}

/// Tries to identify the chip using the given probe.
pub(crate) fn auto_determine_target(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
    tracing::info!("Auto-detecting target");
    let mut found_target = None;

    // Xtensa and RISC-V interfaces don't need moving the probe. For clarity, their
    // handlers work with the borrowed probe, and we use these wrappers to adapt to the
    // ARM way of moving in and out of the probe.
    fn try_detect_riscv_chip_wrapper(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
        try_detect_riscv_chip(&mut probe).map(|found_target| (probe, found_target))
    }

    fn try_detect_xtensa_chip_wrapper(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
        try_detect_xtensa_chip(&mut probe).map(|found_target| (probe, found_target))
    }

    type DetectFn = fn(Probe) -> Result<(Probe, Option<Target>), Error>;
    const ARCHITECTURES: &[DetectFn] = &[
        try_detect_arm_chip,
        try_detect_riscv_chip_wrapper,
        try_detect_xtensa_chip_wrapper,
    ];

    for architecture in ARCHITECTURES {
        let (returned_probe, target) = architecture(probe)?;

        probe = returned_probe;
        if let Some(target) = target {
            tracing::info!("Found target: {}", target.name);
            found_target = Some(target);
            break;
        }
    }

    probe.detach()?;

    Ok((probe, found_target))
}
