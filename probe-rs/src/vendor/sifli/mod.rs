//! SiFli vendor support.

use crate::config::DebugSequence;
use crate::vendor::Vendor;
use probe_rs_target::Chip;

mod sequences;

/// SiFli
#[derive(docsplay::Display)]
pub struct Sifli;

impl Vendor for Sifli {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("SF32LB52") {
            return Some(DebugSequence::Arm(sequences::sf32lb52::Sf32lb52::create()));
        }
        None
    }
}
