//! NXP vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        nxp::sequences::{
            nxp_armv7m::{MIMXRT10xx, MIMXRT11xx},
            nxp_armv8m::{LPC55Sxx, MIMXRT5xxS},
        },
        Vendor,
    },
};

pub mod sequences;

/// NXP
#[derive(docsplay::Display)]
pub struct Nxp;

impl Vendor for Nxp {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("MIMXRT10") {
            DebugSequence::Arm(MIMXRT10xx::create())
        } else if chip.name.starts_with("MIMXRT11") {
            DebugSequence::Arm(MIMXRT11xx::create())
        } else if chip.name.starts_with("MIMXRT5") {
            DebugSequence::Arm(MIMXRT5xxS::create())
        } else if chip.name.starts_with("LPC55S") {
            DebugSequence::Arm(LPC55Sxx::create())
        } else {
            return None;
        };

        Some(sequence)
    }
}
