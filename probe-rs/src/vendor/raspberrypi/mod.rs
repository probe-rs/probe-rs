//! RaspberryPi microcontroller support
use crate::{config::DebugSequence, vendor::Vendor};
use probe_rs_target::Chip;
use sequences::rp235x::Rp235x;
use sequences::rp2040::Rp2040;

pub mod sequences;

/// Raspberry Pi
#[derive(docsplay::Display)]
pub struct RaspberyPi;

impl Vendor for RaspberyPi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("RP2040") {
            DebugSequence::Arm(Rp2040::create())
        } else if chip.name.starts_with("RP235") {
            DebugSequence::Arm(Rp235x::create())
        } else {
            return None;
        };
        Some(sequence)
    }
}
