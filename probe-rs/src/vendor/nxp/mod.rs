//! NXP vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        Vendor,
        nxp::sequences::{
            mcx::MCX,
            nxp_armv6m::LPC80x,
            nxp_armv7m::{MIMXRT10xx, MIMXRT11xx},
            nxp_armv8m::{
                LPC55Sxx, MIMXRT5xxS, MIMXRT118x,
                MIMXRTFamily::{MIMXRT5, MIMXRT6},
                ol23d0::OL23D0,
            },
        },
    },
};

pub mod sequences;

/// NXP
#[derive(docsplay::Display)]
pub struct Nxp;

#[async_trait::async_trait(?Send)]
impl Vendor for Nxp {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("MIMXRT10") {
            DebugSequence::Arm(MIMXRT10xx::create())
        } else if chip.name.starts_with("MIMXRT117") || chip.name.starts_with("MIMXRT116") {
            DebugSequence::Arm(MIMXRT11xx::create())
        } else if chip.name.starts_with("MIMXRT118") {
            DebugSequence::Arm(MIMXRT118x::create())
        } else if chip.name.starts_with("MIMXRT5") {
            DebugSequence::Arm(MIMXRT5xxS::create(MIMXRT5))
        } else if chip.name.starts_with("MIMXRT6") {
            DebugSequence::Arm(MIMXRT5xxS::create(MIMXRT6))
        } else if chip.name.starts_with("LPC55S") {
            DebugSequence::Arm(LPC55Sxx::create())
        } else if chip.name.starts_with("LPC802") || chip.name.starts_with("LPC804") {
            DebugSequence::Arm(LPC80x::create())
        } else if chip.name.starts_with("OL23D0") {
            DebugSequence::Arm(OL23D0::create())
        } else if chip.name.starts_with("MCX") {
            DebugSequence::Arm(MCX::create(chip.name.clone()))
        } else {
            return None;
        };

        Some(sequence)
    }
}
