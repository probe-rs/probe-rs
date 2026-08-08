//! ASR6601 vendor support.

use crate::config::DebugSequence;
use crate::vendor::Vendor;
use probe_rs_target::Chip;

mod sequences;

/// ASR6601
#[derive(docsplay::Display)]
pub struct Asr6601;

impl Vendor for Asr6601 {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let name = chip.name.to_ascii_lowercase();
        if name.contains("asr6601") || name.starts_with("asr-flashalgo") {
            return Some(DebugSequence::Arm(sequences::Asr6601::create()));
        }
        None
    }
}
