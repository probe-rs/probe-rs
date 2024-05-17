//! Silicon Labs vendor support.

use probe_rs_target::Chip;

use crate::{config::DebugSequence, vendor::silabs::sequences::efm32xg2::EFM32xG2};

pub mod sequences;

pub(super) fn try_create_debug_sequence(chip: &Chip) -> Option<DebugSequence> {
    let sequence = if chip.name.starts_with("EFM32PG2")
        || chip.name.starts_with("EFR32BG2")
        || chip.name.starts_with("EFR32FG2")
        || chip.name.starts_with("EFR32MG2")
        || chip.name.starts_with("EFR32ZG2")
    {
        DebugSequence::Arm(EFM32xG2::create())
    } else {
        return None;
    };

    Some(sequence)
}
