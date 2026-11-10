//! Texas Instruments vendor support.

use std::sync::Arc;

use jep106::JEP106Code;
use probe_rs_target::{
    Chip,
    chip_detection::{ChipDetectionMethod, TiIcepickDetection},
};

use crate::{
    Error,
    architecture::arm::{
        ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress, sequences::ArmDebugSequence,
    },
    config::{DebugSequence, Registry},
    probe::{Probe, WireProtocol},
    vendor::{
        Vendor,
        ti::sequences::{
            am261::AM261, cc13xx_cc26xx::CC13xxCC26xx, cc23xx_cc27xx::CC23xxCC27xx, icepick,
            mspm0::MSPM0, tms570::TMS570,
        },
    },
};

pub mod sequences;

/// ICEPick ID bits that identify a chip family; bits 31:28 are the version.
const ICEPICK_ID_MASK: u32 = 0x0fff_ffff;

/// FCFG1 registers that identify a CC13xx/CC26xx.
const FCFG1_USER_ID: u64 = 0x5000_1294;
const FCFG1_ICEPICK_DEVICE_ID: u64 = 0x5000_1318;

fn icepick_detections(registry: &Registry) -> impl Iterator<Item = &TiIcepickDetection> {
    registry
        .families()
        .iter()
        .flat_map(|family| &family.chip_detection)
        .filter_map(|method| match method {
            ChipDetectionMethod::TiIcepick(detection) => Some(detection),
            _ => None,
        })
}

/// Target name from the FCFG1 registers.
fn icepick_chip_name(registry: &Registry, icepick_id: u32, user_id: u32) -> Option<&str> {
    icepick_detections(registry)
        .filter(|detection| detection.icepick_id == icepick_id & ICEPICK_ID_MASK)
        .flat_map(|detection| &detection.variants)
        .find(|variant| user_id & variant.mask == variant.value)
        .map(|variant| variant.target.as_str())
}

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

    fn try_create_arm_detection_sequence(
        &self,
        registry: &Registry,
        probe: &mut Probe,
    ) -> Result<Option<Arc<dyn ArmDebugSequence>>, Error> {
        let Some(protocol) = probe.protocol().filter(|protocol| protocol.is_jtag()) else {
            return Ok(None);
        };
        let Some(mut chain) = probe.try_as_jtag_chain() else {
            return Ok(None);
        };
        let scanned = chain.scan_chain().map(|_| ());
        let idcodes = match scanned {
            Ok(()) if !chain.idcodes().is_empty() => {
                chain.idcodes().iter().flatten().copied().collect()
            }
            // A CC13xx/CC26xx powers up in 2-pin mode and stays silent on TDO, which reads as
            // an error or as an empty chain depending on the probe's pull.
            Ok(()) | Err(_) if protocol == WireProtocol::Jtag => {
                vec![icepick::switch_to_4_pin(&mut chain)?]
            }
            Ok(()) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        tracing::debug!("JTAG ID codes: {idcodes:x?}");

        // The CPU's debug port is only on the scan chain once the ICEPick routes it there,
        // which the default sequence doesn't do.
        let variant = idcodes.iter().find_map(|idcode| {
            icepick_detections(registry)
                .find(|detection| detection.icepick_id == idcode & ICEPICK_ID_MASK)
                .and_then(|detection| detection.variants.first())
        });
        Ok(variant.map(|variant| CC13xxCC26xx::create(variant.target.clone()) as _))
    }

    fn try_identify_arm_chip(
        &self,
        registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
    ) -> Result<Option<String>, Error> {
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut memory = interface.memory_interface(access_port)?;
        let icepick_id = memory.read_word_32(FCFG1_ICEPICK_DEVICE_ID)?;
        let user_id = memory.read_word_32(FCFG1_USER_ID)?;
        tracing::debug!("FCFG1 ICEPICK_DEVICE_ID {icepick_id:#010x}, USER_ID {user_id:#010x}");

        Ok(icepick_chip_name(registry, icepick_id, user_id).map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icepick_chip_names() {
        let registry = Registry::from_builtin_families();
        let name = |icepick_id, user_id| icepick_chip_name(&registry, icepick_id, user_id);

        // Read from a CC2650 and a CC2652R1 LaunchPad.
        assert_eq!(name(0x8b99_a02f, 0x8002_f000), Some("CC2650F128"));
        assert_eq!(name(0x3bb4_102f, 0x3002_f000), Some("CC2652R"));

        // CC1352P: protocol 0xF, CC13 and PA set.
        assert_eq!(name(0x3bb4_102f, 0x3282_f000), Some("CC1352P"));
        // CC2652RB: HPOSC set.
        assert_eq!(name(0x3bb4_102f, 0x3102_f000), Some("CC2652RB"));
        // CC2652PB has no target.
        assert_eq!(name(0x3bb4_102f, 0x3302_f000), None);
        // Unknown family.
        assert_eq!(name(0x0b9b_e02f, 0x3002_f000), None);
    }

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
