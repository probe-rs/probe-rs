//! Vendor support modules.

use std::ops::Deref;

use once_cell::sync::Lazy;
use parking_lot::{Mutex, MutexGuard};
use probe_rs_target::Chip;

use crate::{
    architecture::{
        arm::{sequences::DefaultArmSequence, DpAddress},
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

/// Vendor support trait.
pub trait Vendor: Send + Sync + std::fmt::Display {
    /// Tries to create a debug sequence for the given chip.
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence>;

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

static VENDORS: Lazy<Mutex<Vec<Box<dyn Vendor>>>> = Lazy::new(|| {
    let vendors: Vec<Box<dyn Vendor>> = vec![
        Box::new(microchip::Microchip),
        Box::new(infineon::Infineon),
        Box::new(silabs::SiliconLabs),
        Box::new(ti::TexasInstruments),
        Box::new(espressif::Espressif),
        Box::new(nordicsemi::NordicSemi),
        Box::new(nxp::Nxp),
        Box::new(st::St),
    ];

    Mutex::new(vendors)
});

/// Registers a new vendor.
pub fn register_vendor(vendor: Box<dyn Vendor>) {
    // Order matters. Prepend to allow users to override the default vendors.
    VENDORS.lock().insert(0, vendor);
}

/// Returns a readable view of all known vendors.
fn vendors<'a>() -> impl Deref<Target = [Box<dyn Vendor>]> + 'a {
    MutexGuard::map(VENDORS.lock(), |v| v.as_mut_slice())
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

    // TODO: do not consume probe
    match probe.try_into_arm_interface() {
        Ok(interface) => {
            // We have no information about the target, so we must assume it's using the default DP.
            // We cannot automatically detect DPs if SWD multi-drop is used.
            let dp_address = DpAddress::Default;

            let mut interface = interface
                .initialize(DefaultArmSequence::create(), dp_address)
                .map_err(|(_probe, err)| err)?;

            let found_arm_chip = interface
                .read_chip_info_from_rom_table(dp_address)
                .unwrap_or_else(|error| {
                    tracing::debug!("Error during ARM chip detection: {error}");
                    None
                });

            // TODO: we should probably read the ROM table and pass that to the vendor-specific fn
            if let Some(found_chip) = found_arm_chip.map(ChipInfo::from) {
                found_target = Some(crate::config::get_target_by_chip_info(found_chip)?);
            }

            probe = interface.close();
        }
        Err((returned_probe, DebugProbeError::InterfaceNotAvailable { .. })) => {
            // No ARM interface available.
            tracing::debug!("No ARM interface available, skipping detection.");
            probe = returned_probe;
        }
        Err((returned_probe, error)) => {
            probe = returned_probe;
            tracing::debug!("Error using ARM interface: {error}");
        }
    }

    Ok((probe, found_target))
}

fn try_detect_riscv_chip(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
    let mut found_target = None;

    match probe.try_get_riscv_interface_builder() {
        Ok(factory) => {
            let mut state = factory.create_state();
            let mut interface = factory.attach(&mut state)?;

            if let Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter RISC-V debug mode: {error}");
                return Ok((probe, None));
            }

            match interface.read_idcode() {
                Ok(Some(idcode)) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        if let Some(target_name) =
                            vendor.try_detect_riscv_chip(&mut interface, idcode)?
                        {
                            found_target = Some(registry::get_target_by_name(&target_name)?);
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

    Ok((probe, found_target))
}

fn try_detect_xtensa_chip(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
    let mut found_target = None;

    let mut state = XtensaDebugInterfaceState::default();
    match probe.try_get_xtensa_interface(&mut state) {
        Ok(mut interface) => {
            if Err(error) = interface.enter_debug_mode() {
                tracing::debug!("Failed to enter Xtensa debug mode: {error}");
                return Ok((probe, None));
            }

            match interface.read_idcode() {
                Ok(idcode) => {
                    tracing::debug!("ID code read over JTAG: {idcode:#x}");
                    let vendors = vendors();
                    for vendor in vendors.iter() {
                        if let Some(target_name) =
                            vendor.try_detect_xtensa_chip(&mut interface, idcode)?
                        {
                            found_target = Some(registry::get_target_by_name(&target_name)?);
                            break;
                        }
                    }
                }
                Err(error) => tracing::debug!("Error during Xtensa chip detection: {error}"),
            }

            // TODO: disable debug module
        }

        Err(DebugProbeError::InterfaceNotAvailable { .. }) => {
            tracing::debug!("No Xtensa interface available, skipping detection.");
        }

        Err(error) => {
            tracing::debug!("Error during autodetection of Xtensa chips: {error}");
        }
    }

    Ok((probe, found_target))
}

/// Tries to identify the chip using the given probe.
pub(crate) fn auto_determine_target(mut probe: Probe) -> Result<(Probe, Option<Target>), Error> {
    let mut found_target = None;

    const ARCHITECTURES: &[fn(Probe) -> Result<(Probe, Option<Target>), Error>] = &[
        try_detect_arm_chip,
        try_detect_riscv_chip,
        try_detect_xtensa_chip,
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

    Ok((probe, found_target))
}
