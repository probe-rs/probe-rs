//! HiSilicon RISC-V SoC support (WS63 / Hi3863; BS21/BS2X to follow).
//!
//! These parts run a HiSilicon RISC-V core whose Debug Module sits behind an ARM
//! CoreSight DAP (see [`sequences`]). The RISC-V debug transport is the generic
//! mem-AP DTM; this vendor only supplies the ARM-side debug bring-up.

use crate::{config::DebugSequence, vendor::Vendor};
use probe_rs_target::Chip;

use sequences::Ws63;

pub mod sequences;

/// HiSilicon
#[derive(docsplay::Display)]
pub struct HiSilicon;

impl Vendor for HiSilicon {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // `chip.name` is the variant name (e.g. "WS63"), not the family name.
        if chip.name.starts_with("WS63") {
            Some(DebugSequence::Arm(Ws63::create()))
        } else {
            None
        }
    }
}
