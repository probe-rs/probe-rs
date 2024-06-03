//! Texas Instruments vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{ti::sequences::cc13xx_cc26xx::CC13xxCC26xx, Vendor},
};

pub mod sequences;

/// Texas Instruments
#[derive(docsplay::Display)]
pub struct TexasInstruments;

impl Vendor for TexasInstruments {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("CC13") || chip.name.starts_with("CC26") {
            DebugSequence::Arm(CC13xxCC26xx::create(chip.name.clone()))
        } else {
            return None;
        };

        Some(sequence)
    }
}
