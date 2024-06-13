//! Nordic Semiconductor vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        nordicsemi::sequences::{nrf52::Nrf52, nrf53::Nrf5340, nrf91::Nrf9160},
        Vendor,
    },
};

pub mod sequences;

/// Nordic Semiconductor
#[derive(docsplay::Display)]
pub struct NordicSemi;

impl Vendor for NordicSemi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("nRF5340") {
            DebugSequence::Arm(Nrf5340::create())
        } else if chip.name.starts_with("nRF52") {
            DebugSequence::Arm(Nrf52::create())
        } else if chip.name.starts_with("nRF9160") {
            DebugSequence::Arm(Nrf9160::create())
        } else {
            return None;
        };

        Some(sequence)
    }
}
