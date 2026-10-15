//! NSING vendor support.
//!
//! The vendor renamed from Nationstech to NSING in September 2025.
//! Their CMSIS-Packs and flash algorithms still carry the old name
//! (`Nationstech.N32H7xx_DFP`), so a target description generated from one will
//! spell it that way — that is expected, and not a second vendor.

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
        // Every N32H7xx part needs this sequence, so match the family rather than
        // individual part numbers. Case-folded because the vendor's own device list
        // mixes cases (`N32H760xIx7` next to `N32H785XIB7EC`).
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

    /// The family match is a name prefix and nothing else, so it is the only
    /// thing standing between an N32H7xx part and no sequence at all.
    ///
    /// Parts from all three families are listed on purpose: 73x and 76x are
    /// single-core and 78x is dual-core, and the sequence is expected to take
    /// all of them.
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

    /// The other half of the same prefix: it has to be narrow enough to leave the
    /// vendor's *own* other families alone, because nothing else in the registry
    /// would claim them — a stray match here means a part that gets a sequence it
    /// was never meant to have.
    ///
    /// `N32H47x` is the sharpest of the four: it is one digit away from matching,
    /// and a real part, not a hypothetical one (the vendor ships
    /// `N32H47x_256.FLM` for it). The rest are the automotive, general-purpose
    /// and low-power lines.
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
