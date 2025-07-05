//! Silicon Labs vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{Vendor, silabs::sequences::efm32xg2::EFM32xG2},
};

pub mod sequences;

/// Silicon Labs
#[derive(docsplay::Display)]
pub struct SiliconLabs;

impl Vendor for SiliconLabs {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("EFM32PG2")
            || chip.name.starts_with("EFR32BG2")
            || chip.name.starts_with("EFR32FG2")
            || chip.name.starts_with("EFR32MG2")
            || chip.name.starts_with("EFR32ZG2")
        {
            DebugSequence::Arm(EFM32xG2::create(chip))
        } else {
            return None;
        };

        Some(sequence)
    }
}
