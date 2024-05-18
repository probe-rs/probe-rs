//! Espressif vendor support.

use probe_rs_target::Chip;

use crate::{
    config::DebugSequence,
    vendor::{
        espressif::sequences::{
            esp32::ESP32, esp32c2::ESP32C2, esp32c3::ESP32C3, esp32c6::ESP32C6, esp32h2::ESP32H2,
            esp32s2::ESP32S2, esp32s3::ESP32S3,
        },
        Vendor,
    },
};

pub mod sequences;

/// Espressif
#[derive(docsplay::Display)]
pub struct Espressif;

impl Vendor for Espressif {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("esp32-") {
            DebugSequence::Xtensa(ESP32::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32s2") {
            DebugSequence::Xtensa(ESP32S2::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32s3") {
            DebugSequence::Xtensa(ESP32S3::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c2") {
            DebugSequence::Riscv(ESP32C2::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c3") {
            DebugSequence::Riscv(ESP32C3::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32c6") {
            DebugSequence::Riscv(ESP32C6::create(chip))
        } else if chip.name.eq_ignore_ascii_case("esp32h2") {
            DebugSequence::Riscv(ESP32H2::create(chip))
        } else {
            return None;
        };

        Some(sequence)
    }
}
