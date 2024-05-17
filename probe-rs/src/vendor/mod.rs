//! Vendor support modules.

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

/// Tries to create a debug sequence for the given chip.
pub fn try_create_debug_sequence(chip: &Chip) -> Option<DebugSequence> {
    const VENDOR_SEQUENCES: &[fn(&Chip) -> Option<DebugSequence>] = &[
        espressif::try_create_debug_sequence,
        infineon::try_create_debug_sequence,
        microchip::try_create_debug_sequence,
        nordicsemi::try_create_debug_sequence,
        nxp::try_create_debug_sequence,
        silabs::try_create_debug_sequence,
        st::try_create_debug_sequence,
        ti::try_create_debug_sequence,
    ];

    for sequence in VENDOR_SEQUENCES {
        if let Some(sequence) = sequence(chip) {
            return Some(sequence);
        }
    }

    None
}
