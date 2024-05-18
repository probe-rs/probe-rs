//! Vendor support modules.

use std::ops::Deref;

use once_cell::sync::Lazy;
use parking_lot::{Mutex, MutexGuard};
use probe_rs_target::Chip;

use crate::{
    architecture::{
        arm::{sequences::DefaultArmSequence, DpAddress},
        xtensa::communication_interface::XtensaDebugInterfaceState,
    },
    config::{ChipInfo, DebugSequence},
    probe::{DebugProbeError, Probe},
    Error,
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

fn try_detect_arm_chip(mut probe: Probe) -> Result<(Probe, Option<ChipInfo>), Error> {
    let mut found_chip = None;

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
                .unwrap_or_else(|e| {
                    tracing::info!("Error during auto-detection of ARM chips: {}", e);
                    None
                });

            found_chip = found_arm_chip.map(ChipInfo::from);

            probe = interface.close();
        }
        Err((returned_probe, DebugProbeError::InterfaceNotAvailable { .. })) => {
            // No ARM interface available.
            tracing::debug!("No ARM interface available, skipping detection.");
            probe = returned_probe;
        }
        Err((returned_probe, err)) => {
            probe = returned_probe;
            tracing::debug!("Error using ARM interface: {}", err);
        }
    }

    Ok((probe, found_chip))
}

fn try_detect_riscv_chip(mut probe: Probe) -> Result<(Probe, Option<ChipInfo>), Error> {
    match probe.try_get_riscv_interface_builder() {
        Ok(factory) => {
            let mut state = factory.create_state();
            let mut interface = factory.attach(&mut state)?;
            let idcode = interface.read_idcode();

            tracing::debug!("ID Code read over JTAG: {:x?}", idcode);
        }

        Err(DebugProbeError::InterfaceNotAvailable { .. }) => {
            tracing::debug!("No RISC-V interface available, skipping detection.");
        }

        Err(err) => {
            tracing::debug!("Error during autodetection of RISC-V chips: {}", err);
        }
    }

    Ok((probe, None))
}

fn try_detect_xtensa_chip(mut probe: Probe) -> Result<(Probe, Option<ChipInfo>), Error> {
    let mut state = XtensaDebugInterfaceState::default();
    match probe.try_get_xtensa_interface(&mut state) {
        Ok(mut interface) => {
            let idcode = interface.read_idcode();

            tracing::debug!("ID Code read over JTAG: {:x?}", idcode);
        }

        Err(DebugProbeError::InterfaceNotAvailable { .. }) => {
            tracing::debug!("No Xtensa interface available, skipping detection.");
        }

        Err(err) => {
            tracing::debug!("Error during autodetection of Xtensa chips: {}", err);
        }
    }

    Ok((probe, None))
}

/// Tries to identify the chip using the given probe.
pub(crate) fn auto_determine_target(mut probe: Probe) -> Result<(Probe, Option<ChipInfo>), Error> {
    let mut found_chip = None;

    const ARCHITECTURES: &[fn(Probe) -> Result<(Probe, Option<ChipInfo>), Error>] = &[
        try_detect_arm_chip,
        try_detect_riscv_chip,
        try_detect_xtensa_chip,
    ];

    for method in ARCHITECTURES {
        let (returned_probe, chip) = method(probe)?;

        probe = returned_probe;
        if chip.is_some() {
            found_chip = chip;
            break;
        }
    }

    Ok((probe, found_chip))
}
