//! Texas Instruments vendor support.

use jep106::JEP106Code;
use probe_rs_target::Chip;

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

/// PARTID bits 23:0, variant and part, from driverlib's chipinfo.h.
const CC23XX_CC27XX_PARTS: &[(u32, &str)] = &[
    (0x01_2DDA, "CC2340R5"), // CC2340R52E0RGER
    (0x3B_2DDA, "CC2340R5"), // CC2340R52E0RKPR
    (0x0F_2DDA, "CC2340R5"), // CC2340R52E0WRHBRQ1
    (0xE8_AF23, "CC2745R10Q1"),
    (0x9D_6301, "CC2755P10"), // CC2755R105E0WRHAR, same memory
    (0xE6_6301, "CC2755P10"), // CC2755P105E0WRHAR
];

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
        _registry: &Registry,
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

        Ok(CC23XX_CC27XX_PARTS
            .iter()
            .find(|(part, _)| *part == part_id & 0xFF_FFFF)
            .map(|(_, name)| name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cc23xx_cc27xx_parts_are_targets() {
        let registry = Registry::from_builtin_families();
        for (_, name) in CC23XX_CC27XX_PARTS {
            assert!(registry.get_target_by_name(name).is_ok(), "{name}");
        }
    }
}
