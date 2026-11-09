//! Texas Instruments vendor support.

use jep106::JEP106Code;
use probe_rs_target::{Chip, chip_detection::ChipDetectionMethod};

use crate::{
    Error,
    architecture::arm::{ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress},
    config::{DebugSequence, Registry},
    vendor::{
        Vendor,
        ti::sequences::{
            am261::AM261, cc13xx_cc26xx::CC13xxCC26xx, cc23xx_cc27xx::CC23xxCC27xx, mspm0::MSPM0,
            tms570::TMS570,
        },
    },
};

pub mod sequences;

const JEP_TI: JEP106Code = JEP106Code::new(0x0, 0x17);

/// CC23xx/CC27xx CFG-AP: DEVICEID (manufacturer in bits 11:0) and PARTID.
const CFG_AP: u8 = 1;
const CFG_AP_DEVICEID: u64 = 0x0;
const CFG_AP_PARTID: u64 = 0x4;
const DEVICEID_TI: u32 = 0x02F;

/// Target name from the PARTID variant and part fields (bits 23:0).
fn cfg_ap_chip_name(registry: &Registry, part_id: u32) -> Option<&str> {
    registry
        .families()
        .iter()
        .flat_map(|family| &family.chip_detection)
        .find_map(|method| match method {
            ChipDetectionMethod::TiCfgAp(detection) => {
                detection.variants.get(&(part_id & 0xFF_FFFF))
            }
            _ => None,
        })
        .map(String::as_str)
}

/// Texas Instruments
#[derive(docsplay::Display)]
pub struct TexasInstruments;

impl Vendor for TexasInstruments {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("CC13") || chip.name.starts_with("CC26") {
            DebugSequence::Arm(CC13xxCC26xx::create(chip.name.clone()))
        } else if chip.name.starts_with("CC23") || chip.name.starts_with("CC27") {
            DebugSequence::Arm(CC23xxCC27xx::create(chip.name.clone()))
        } else if chip.name.starts_with("TMS570") {
            DebugSequence::Arm(TMS570::create(chip.name.clone()))
        } else if chip.name.starts_with("AM261") {
            DebugSequence::Arm(AM261::create())
        } else if chip.name.starts_with("MSPM0") || chip.name.starts_with("MSPS") {
            DebugSequence::Arm(MSPM0::create(chip.name.clone()))
        } else {
            return None;
        };

        Some(sequence)
    }

    fn try_detect_arm_chip(
        &self,
        registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if chip_info.manufacturer != JEP_TI {
            return Ok(None);
        }

        let cfg_ap = FullyQualifiedApAddress::v1_with_default_dp(CFG_AP);
        let Ok(device_id) = interface.read_raw_ap_register(&cfg_ap, CFG_AP_DEVICEID) else {
            return Ok(None);
        };
        if device_id & 0xFFF != DEVICEID_TI {
            return Ok(None);
        }
        let Ok(part_id) = interface.read_raw_ap_register(&cfg_ap, CFG_AP_PARTID) else {
            return Ok(None);
        };
        tracing::debug!("CC23xx/CC27xx: device ID {device_id:#010x}, part ID {part_id:#010x}");

        Ok(cfg_ap_chip_name(registry, part_id).map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cfg_ap_chip_names() {
        let registry = Registry::from_builtin_families();

        // Read from a CC2755P10 LaunchPad.
        assert_eq!(cfg_ap_chip_name(&registry, 0x80e6_6301), Some("CC2755P10"));
        assert_eq!(cfg_ap_chip_name(&registry, 0x80e6_6302), None);

        for family in registry.families() {
            for method in &family.chip_detection {
                let ChipDetectionMethod::TiCfgAp(detection) = method else {
                    continue;
                };
                for name in detection.variants.values() {
                    assert!(registry.get_target_by_name(name).is_ok(), "{name}");
                }
            }
        }
    }
}
