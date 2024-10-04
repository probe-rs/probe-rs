//! Raspberry Pi vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        raspberrypi::sequences::rp2040::{Rp2040, Rp2040Rescue},
        Vendor,
    },
};

pub mod sequences;

/// Raspberry Pi
#[derive(docsplay::Display)]
pub struct RaspberryPi;

impl Vendor for RaspberryPi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.eq_ignore_ascii_case("rp2040-rescue") {
            DebugSequence::Arm(Rp2040Rescue::create())
        } else if chip.name.starts_with("RP2040") {
            DebugSequence::Arm(Rp2040::create())
        } else {
            return None;
        };

        Some(sequence)
    }
}
