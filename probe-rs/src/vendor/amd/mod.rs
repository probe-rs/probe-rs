//! AMD SoC support (formerly Xilinx).
//!
//! Does not support non-Arm AMD CPUs.

use crate::{config::DebugSequence, vendor::Vendor};
use probe_rs_target::Chip;
use sequences::x7z::X7Z;

pub mod sequences;

/// AMD
#[derive(docsplay::Display)]
pub struct Amd;

impl Vendor for Amd {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("X7Z") {
            Some(DebugSequence::Arm(X7Z::create()))
        } else {
            None
        }
    }
}
