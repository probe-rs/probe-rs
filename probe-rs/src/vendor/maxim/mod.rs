//! Maxim Integrated / Analog Devices vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        Vendor,
        maxim::sequences::max32::Max32,
    },
};

/// Debug sequences for Maxim chips.
pub mod sequences;

/// Maxim Integrated / Analog Devices
#[derive(docsplay::Display)]
pub struct Maxim;

impl Vendor for Maxim {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // Use VECTRESET for MAX32690, while other MAX32xxx chips (MAX32650, MAX32660,
        // MAX32665) work with the default SYSRESETREQ-based sequence.
        let needs_vectreset = matches!(chip.name.as_str(), "MAX32690");

        if needs_vectreset {
            Some(DebugSequence::Arm(Max32::create()))
        } else {
            None
        }
    }
}
