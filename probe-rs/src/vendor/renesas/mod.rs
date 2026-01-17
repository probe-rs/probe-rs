//! Renesas vendor support.

use probe_rs_target::{Chip, chip_detection::ChipDetectionMethod};

use crate::{
    Error,
    architecture::arm::{
        ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress,
        dp::{DpRegister as _, TARGETID},
    },
    config::{DebugSequence, Registry},
    vendor::Vendor,
};

/// Renesas
#[derive(docsplay::Display)]
pub struct Renesas;

impl Vendor for Renesas {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
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

        let target_id = TARGETID(
            interface
                .read_raw_dp_register(interface.current_debug_port().unwrap(), TARGETID::ADDRESS)?,
        );
        let target_pn = target_id.tpartno();

        let mut part_number = [0_u8; 16];

        for family in registry.families() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_renesas_fmifrt)
            {
                if target_pn != info.target_id {
                    continue;
                }

                interface
                    .memory_interface(access_port)?
                    .read_8(info.mcu_pn_base as _, &mut part_number)?;

                let Ok(part_number) = std::str::from_utf8(&part_number) else {
                    continue;
                };

                let part_number = part_number.trim();

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
