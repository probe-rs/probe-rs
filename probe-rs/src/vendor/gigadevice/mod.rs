//! GigaDevice vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{Vendor, gigadevice::sequences::Gd32H7Sequence},
};

pub mod sequences;

/// GigaDevice.
#[derive(docsplay::Display)]
pub struct GigaDevice;

impl Vendor for GigaDevice {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("GD32H7") {
            Some(DebugSequence::Arm(Gd32H7Sequence::create()))
        } else {
            None
        }
    }
}
