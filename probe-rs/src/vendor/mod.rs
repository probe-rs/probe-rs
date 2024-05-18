//! Vendor support modules.

use std::ops::Deref;

use once_cell::sync::Lazy;
use parking_lot::{Mutex, MutexGuard};
use probe_rs_target::Chip;

use crate::config::DebugSequence;

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
