//! Microchip vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{microchip::sequences::atsam::AtSAM, Vendor},
};

pub mod sequences;

/// Microchip
#[derive(docsplay::Display)]
pub struct Microchip;

impl Vendor for Microchip {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("ATSAMD1")
            || chip.name.starts_with("ATSAMD2")
            || chip.name.starts_with("ATSAMDA")
            || chip.name.starts_with("ATSAMD5")
            || chip.name.starts_with("ATSAME5")
        {
            DebugSequence::Arm(AtSAM::create())
        } else {
            return None;
        };

        Some(sequence)
    }
}
