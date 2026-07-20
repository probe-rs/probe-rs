//! Maxim Integrated / Analog Devices vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        Vendor,
        maxim::sequences::max32::Max32,
    },
};

/// Debug sequences for Maxim chips.
pub mod sequences;

/// Maxim Integrated / Analog Devices
#[derive(docsplay::Display)]
pub struct Maxim;

impl Vendor for Maxim {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        // Map chip names to ROM breakpoint addresses.  Add new entries here
        // when adding support for additional MAX32xxx variants.
        let rom_bp = match chip.name.as_str() {
            "MAX32690" => Some(0x0000_FFF4),
            "MAX32670" | "MAX32675" => Some(0x0000_2174),
            _ => None,
        };

        rom_bp.map(|addr| DebugSequence::Arm(Max32::create(addr)))
    }
}
