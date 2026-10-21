//! NSING vendor support.
//!
//! Nationstech changed its name to NSING in September 2025. The CMSIS-Packs
//! and flash algorithms still use the old name (`Nationstech.N32H7xx_DFP`).

use probe_rs_target::Chip;

use crate::{config::DebugSequence, vendor::Vendor};

#[cfg(test)]
use probe_rs_target::CoreType;

pub mod sequences;

/// NSING
#[derive(docsplay::Display)]
pub struct Nsing;

impl Vendor for Nsing {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // All N32H7xx parts use this sequence. The vendor device list mixes
        // cases (`N32H760xIx7`, `N32H785XIB7EC`), so ignore case.
        if chip.name.to_ascii_uppercase().starts_with("N32H7") {
            return Some(DebugSequence::Arm(
                sequences::n32h7xx::N32H7xx::create_for_chip(chip),
            ));
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chip(name: &str) -> Chip {
        Chip::generic_arm(name, CoreType::Armv7em)
    }

    /// The sequence must match the single-core 73x/76x and the dual-core 78x
    /// parts.
    #[test]
    fn picks_up_n32h7_parts() {
        for name in [
            "N32H730xIx7",
            "N32H760xIx7",
            "N32H762xIx7",
            "N32H765xIx7EC",
            "N32H785xIx7",
            "N32H785XIB7EC",
            "N32H787xIx7",
        ] {
            assert!(
                Nsing.try_create_debug_sequence(&chip(name)).is_some(),
                "{name} should use the Nsing sequence"
            );
        }
    }

    /// The sequence must not match the other vendor families (automotive,
    /// general-purpose, low-power). `N32H47x` is a real part with a flash
    /// algorithm (`N32H47x_256.FLM`), and its name
    /// is one digit away from a match.
    #[test]
    fn leaves_the_vendors_other_families_alone() {
        for name in ["N32H47x", "N32G45x", "N32L40x", "N32A455"] {
            assert!(
                Nsing.try_create_debug_sequence(&chip(name)).is_none(),
                "{name} should not use the N32H7xx sequence"
            );
        }
    }
}
