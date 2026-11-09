//! Texas Instruments vendor support.

use std::sync::Arc;

use probe_rs_target::{
    Chip,
    chip_detection::{ChipDetectionMethod, TiIcepickDetection},
};

use crate::{
    Error,
    architecture::arm::{ArmDebugInterface, FullyQualifiedApAddress, sequences::ArmDebugSequence},
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

    fn try_create_arm_detection_sequence(
        &self,
        registry: &Registry,
        probe: &mut Probe,
    ) -> Result<Option<Arc<dyn ArmDebugSequence>>, Error> {
        if probe.protocol() != Some(WireProtocol::Jtag) {
            return Ok(None);
        }
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
            Ok(()) | Err(_) => vec![icepick::switch_to_4_pin(&mut chain)?],
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
    use super::icepick_chip_name;
    use crate::config::Registry;

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
}
