use probe_rs_target::{Chip, chip_detection::ChipDetectionMethod};

use crate::{
    Error,
    architecture::arm::{ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress},
    config::{DebugSequence, Registry},
    vendor::Vendor,
};

/// Renesas
#[derive(docsplay::Display)]
pub struct Renesas;

impl Vendor for Renesas {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        None
    }

    fn try_detect_arm_chip(
        &self,
        registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if chip_info.manufacturer.get() != Some("Renesas Electronics") {
            return Ok(None);
        }

        // FIXME: This is a bit shaky but good enough for now.
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);

        const FMIFRT_BASE: u64 = 0x0100_3C00;

        let mut part_number = [0_u8; 16];

        interface
            .memory_interface(access_port)?
            .read_8(FMIFRT_BASE + 0x24, &mut part_number)?;

        let part_number = std::str::from_utf8(&part_number).unwrap().trim();

        for family in registry.families() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_renesas_fmifrt)
            {
                for (target, variants) in info.variants.iter() {
                    if variants.iter().any(|v| v == part_number) {
                        return Ok(Some(target.clone()));
                    }
                }
            }
        }

        Ok(None)
    }
}
