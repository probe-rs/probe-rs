//! Espressif device support for probe-rs

use probe_rs::plugin;

pub fn register_plugin() {
    plugin::register_plugin(plugin::Plugin {
        vendors: &[&Espressif],
    });
}

use probe_rs_target::{
    Chip,
    chip_detection::{ChipDetectionMethod, EspressifDetection},
};

use probe_rs::{
    Error, MemoryInterface,
    architecture::{
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    config::{DebugSequence, Registry},
    vendor::Vendor,
};
use sequences::{
    esp32::ESP32, esp32c2::ESP32C2, esp32c3::ESP32C3, esp32c5::ESP32C5, esp32c6::ESP32C6,
    esp32c61::ESP32C61, esp32h2::ESP32H2, esp32p4::ESP32P4, esp32s2::ESP32S2, esp32s3::ESP32S3,
};

pub mod sequences;

// A magic number that resides in the ROM of Espressif chips. This points to 4 bytes that are mostly
// unique to each chip variant. There may be some overlap between revisions (e.g. esp32c3)
// and chips may be placed on modules that are configured significantly
// differently (esp32 with 1.8V or 3.3V VDD_SDIO).
// See:
// - https://github.com/esp-rs/espflash/blob/5c898ac7a37fd6ec7d7c4562585818ac878e5a2f/espflash/src/flasher/stubs.rs#L23
// - https://github.com/esp-rs/espflash/blob/5c898ac7a37fd6ec7d7c4562585818ac878e5a2f/espflash/src/flasher/mod.rs#L589-L590
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
    registry: &Registry,
    mut read_magic: impl FnMut(u64) -> Option<u32>,
    idcode: u32,
) -> Option<String> {
    for family in registry.families() {
        for info in family
            .chip_detection
            .iter()
            .filter_map(ChipDetectionMethod::as_espressif)
        {
            if info.idcode != idcode {
                continue;
            }
            if let Some(target) = get_target_by_magic(info, 0) {
                // C5/P4 workaround - CPU is on TAP 1. We can infer this from the family,
                // but we can't use it in the detection process.
                return Some(target);
            } else {
                let Some(read_magic) = read_magic(MAGIC_VALUE_ADDRESS) else {
                    continue;
                };
                tracing::debug!("Read magic value: {read_magic:#010x}");
                if let Some(target) = get_target_by_magic(info, read_magic) {
                    return Some(target);
                }
            }
        }
    }

    None
}

/// Espressif
#[derive(docsplay::Display)]
struct Espressif;

impl Vendor for Espressif {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.eq_ignore_ascii_case("esp32s2") {
            DebugSequence::Xtensa(ESP32S2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32s3") {
            DebugSequence::Xtensa(ESP32S3::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c2") {
            DebugSequence::Riscv(ESP32C2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c3") {
            DebugSequence::Riscv(ESP32C3::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c5") {
            DebugSequence::Riscv(ESP32C5::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c61") {
            DebugSequence::Riscv(ESP32C61::create())
        } else if chip.name.eq_ignore_ascii_case("esp32c6") {
            DebugSequence::Riscv(ESP32C6::create())
        } else if chip.name.eq_ignore_ascii_case("esp32h2") {
            DebugSequence::Riscv(ESP32H2::create())
        } else if chip.name.eq_ignore_ascii_case("esp32p4") {
            DebugSequence::Riscv(ESP32P4::create())
        } else if chip.name.starts_with("esp32") {
            DebugSequence::Xtensa(ESP32::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    fn try_detect_riscv_chip(
        &self,
        registry: &Registry,
        probe: &mut RiscvCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(try_detect_espressif_chip(
            registry,
            |address| {
                probe
                    .halted_access(|probe| Ok(probe.read_word_32(address).ok()))
                    .unwrap()
            },
            idcode,
        ))
    }

    fn try_detect_xtensa_chip(
        &self,
        registry: &Registry,
        probe: &mut XtensaCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(try_detect_espressif_chip(
            registry,
            |address| probe.read_word_32(address).ok(),
            idcode,
        ))
    }
}
