//! Silicon Labs vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        Vendor,
        silabs::sequences::{efm32xg1::EFM32xG1, efm32xg2::EFM32xG2},
    },
};

pub mod sequences;

/// Silicon Labs
#[derive(docsplay::Display)]
pub struct SiliconLabs;

impl Vendor for SiliconLabs {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // Series 2 (Cortex-M33, ARMv8-M). Checked first so e.g. EFR32MG21 does not
        // get caught by the Series 1 "EFR32MG1" prefix.
        let sequence = if chip.name.starts_with("EFM32PG2")
            || chip.name.starts_with("EFR32BG2")
            || chip.name.starts_with("EFR32FG2")
            || chip.name.starts_with("EFR32MG2")
            || chip.name.starts_with("EFR32ZG2")
        {
            DebugSequence::Arm(EFM32xG2::create(chip))
        }
        // Series 1 (Cortex-M3/M4, ARMv7-M). The "G1" prefixes also cover the
        // G12/G13/G14 variants. These need an FPB-breakpoint reset catch; the
        // default ARM sequence's vector catch does not reliably halt them.
        else if chip.name.starts_with("EFM32PG1")
            || chip.name.starts_with("EFM32JG1")
            || chip.name.starts_with("EFR32BG1")
            || chip.name.starts_with("EFR32FG1")
            || chip.name.starts_with("EFR32MG1")
            || chip.name.starts_with("EFR32ZG1")
        {
            DebugSequence::Arm(EFM32xG1::create(chip))
        } else {
            return None;
        };

        Some(sequence)
    }
}
