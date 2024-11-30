//! Espressif vendor support.

use probe_rs_target::{
    Chip,
    chip_detection::{ChipDetectionMethod, EspressifDetection},
};

use crate::{
    Error, MemoryInterface,
    architecture::{
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::XtensaCommunicationInterface,
    },
    config::{DebugSequence, Registry},
    vendor::{
        Vendor,
        espressif::sequences::{
            esp32::ESP32, esp32c2::ESP32C2, esp32c3::ESP32C3, esp32c6::ESP32C6, esp32h2::ESP32H2,
            esp32s2::ESP32S2, esp32s3::ESP32S3,
        },
    },
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

async fn try_detect_espressif_chip(
    registry: &Registry,
    probe: &mut impl MemoryInterface,
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
            let Ok(read_magic) = probe.read_word_32(MAGIC_VALUE_ADDRESS).await else {
                continue;
            };
            tracing::debug!("Read magic value: {read_magic:#010x}");
            if let Some(target) = get_target_by_magic(info, read_magic) {
                return Some(target);
            }
        }
    }

    None
}

/// Espressif
#[derive(docsplay::Display)]
pub struct Espressif;

#[async_trait::async_trait(?Send)]
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
        } else if chip.name.eq_ignore_ascii_case("esp32c6") {
            DebugSequence::Riscv(ESP32C6::create())
        } else if chip.name.eq_ignore_ascii_case("esp32h2") {
            DebugSequence::Riscv(ESP32H2::create())
        } else if chip.name.starts_with("esp32") {
            DebugSequence::Xtensa(ESP32::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    async fn try_detect_riscv_chip(
        &self,
        registry: &Registry,
        probe: &mut RiscvCommunicationInterface<'_>,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        let result = probe
            .halted_access(async |probe| {
                Ok(try_detect_espressif_chip(registry, probe, idcode).await)
            })
            .await?;

        Ok(result)
    }

    async fn try_detect_xtensa_chip(
        &self,
        registry: &Registry,
        probe: &mut XtensaCommunicationInterface<'_>,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(try_detect_espressif_chip(registry, probe, idcode).await)
    }
}
