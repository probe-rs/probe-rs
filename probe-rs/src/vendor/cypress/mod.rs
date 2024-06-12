//! Cypress vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{cypress::sequences::fm3::FM3, Vendor},
};

pub mod sequences;

/// Cypress
#[derive(docsplay::Display)]
pub struct Cypress;

impl Vendor for Cypress {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("FM3") {
            DebugSequence::Arm(FM3::create())
        } else {
            return None;
        };

        Some(sequence)
    }
}
