//! Espressif vendor support.

use probe_rs_target::{
    chip_detection::{ChipDetectionMethod, EspressifDetection},
    Chip,
};

use crate::{
    architecture::{
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    config::{registry, DebugSequence},
    vendor::{
        espressif::sequences::{
            esp32::ESP32, esp32c2::ESP32C2, esp32c3::ESP32C3, esp32c6::ESP32C6, esp32h2::ESP32H2,
            esp32s2::ESP32S2, esp32s3::ESP32S3,
        },
        Vendor,
    },
    Error, MemoryInterface,
};

pub mod sequences;

const MAGIC_VALUE_ADDRESS: u64 = 0x4000_1000;

fn get_target_by_magic(info: &EspressifDetection, read_magic: u32) -> Option<String> {
    for (magic, target) in info.variants.iter() {
        if *magic == read_magic {
            return Some(target.clone());
        }
    }
    None
}

fn try_detect_espressif_chip(
    probe: &mut impl MemoryInterface,
    idcode: u32,
) -> Result<Option<String>, Error> {
    let families = registry::families_ref();
    for family in families.into_iter() {
        for info in family
            .chip_detection
            .iter()
            .filter_map(ChipDetectionMethod::as_espressif)
        {
            if info.idcode != idcode {
                continue;
            }
            let Ok(read_magic) = probe.read_word_32(MAGIC_VALUE_ADDRESS) else {
                continue;
            };
            if let Some(target) = get_target_by_magic(info, read_magic) {
                return Ok(Some(target));
            }
        }
    }

    Ok(None)
}

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

    fn try_detect_riscv_chip(
        &self,
        probe: &mut RiscvCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        try_detect_espressif_chip(probe, idcode)
    }

    fn try_detect_xtensa_chip(
        &self,
        probe: &mut XtensaCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        try_detect_espressif_chip(probe, idcode)
    }
}
