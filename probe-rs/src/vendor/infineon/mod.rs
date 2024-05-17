//! Infineon vendor support.

use probe_rs_target::Chip;

use crate::{config::DebugSequence, vendor::infineon::sequences::xmc4000::XMC4000};

pub mod sequences;

pub(super) fn try_create_debug_sequence(chip: &Chip) -> Option<DebugSequence> {
    let sequence = if chip.name.starts_with("XMC4") {
        DebugSequence::Arm(XMC4000::create())
    } else {
        return None;
    };

    Some(sequence)
}
