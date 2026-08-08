//! ASR Microelectronics vendor support.

use crate::config::DebugSequence;
use crate::vendor::Vendor;
use probe_rs_target::Chip;

mod sequences;

/// ASR Microelectronics
#[derive(docsplay::Display)]
pub struct Asrmicro;

impl Vendor for Asrmicro {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let name = chip.name.to_ascii_lowercase();
        if name.contains("asr6601") {
            return Some(DebugSequence::Arm(sequences::Asr6601::create()));
        }
        None
    }
}
