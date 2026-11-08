//! Texas Instruments vendor support.

use std::sync::Arc;

use probe_rs_target::Chip;

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

/// ICEPick ID codes (version bits masked) of the CC13xx/CC26xx families, with a chip of each
/// family for the debug sequence. The CPU's debug port is only on the scan chain once the
/// ICEPick routes it there, which the default sequence doesn't do.
const ICEPICK_FAMILIES: &[(u32, &str)] = &[
    (ICEPICK_CC26X0, "CC2650F128"),
    (ICEPICK_CC13X2_CC26X2, "CC2652R"),
];
const ICEPICK_CC26X0: u32 = 0x0b99_a02f;
const ICEPICK_CC13X2_CC26X2: u32 = 0x0bb4_102f;
const ICEPICK_ID_MASK: u32 = 0x0fff_ffff;

/// FCFG1 registers that identify a CC13xx/CC26xx.
const FCFG1_USER_ID: u64 = 0x5000_1294;
const FCFG1_ICEPICK_DEVICE_ID: u64 = 0x5000_1318;

/// Chip name from the FCFG1 registers, as in TI's driverlib (`ChipInfo_GetChipType()`).
fn cc13xx_cc26xx_name(icepick_id: u32, user_id: u32) -> Option<&'static str> {
    let protocol = (user_id >> 12) & 0xf;
    let cc13 = user_id & (1 << 23) != 0;
    let hposc = user_id & (1 << 24) != 0;
    let pa = user_id & (1 << 25) != 0;

    match icepick_id & ICEPICK_ID_MASK {
        // The CC26x0 variants share the 128 KB flash layout.
        ICEPICK_CC26X0 => Some("CC2650F128"),
        ICEPICK_CC13X2_CC26X2 => match (protocol, cc13, pa, hposc) {
            (0xf, true, true, _) => Some("CC1352P"),
            (0xf, true, false, _) => Some("CC1352R"),
            (0xf, false, true, false) => Some("CC2652P"),
            (0xf, false, false, true) => Some("CC2652RB"),
            (0xf, false, false, false) => Some("CC2652R"),
            (0x9, _, false, _) => Some("CC2642R"),
            (0x8, _, false, _) => Some("CC1312R"),
            _ => None,
        },
        _ => None,
    }
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
        probe: &mut Probe,
    ) -> Result<Option<Arc<dyn ArmDebugSequence>>, Error> {
        if probe.protocol() != Some(WireProtocol::Jtag) {
            return Ok(None);
        }
        let Some(mut chain) = probe.try_as_jtag_chain() else {
            return Ok(None);
        };
        let idcodes = match chain.scan_chain() {
            Ok(_) => chain.idcodes().iter().flatten().copied().collect(),
            // A CC13xx/CC26xx powers up in 2-pin mode and stays silent on TDO.
            Err(_) => vec![icepick::switch_to_4_pin(&mut chain)?],
        };
        tracing::debug!("JTAG ID codes: {idcodes:x?}");

        let family = idcodes.iter().find_map(|idcode| {
            ICEPICK_FAMILIES
                .iter()
                .find(|(id, _)| idcode & ICEPICK_ID_MASK == *id)
        });
        Ok(family.map(|(_, name)| CC13xxCC26xx::create(name.to_string()) as _))
    }

    fn try_identify_arm_chip(
        &self,
        _registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
    ) -> Result<Option<String>, Error> {
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut memory = interface.memory_interface(access_port)?;
        let icepick_id = memory.read_word_32(FCFG1_ICEPICK_DEVICE_ID)?;
        let user_id = memory.read_word_32(FCFG1_USER_ID)?;
        tracing::debug!("FCFG1 ICEPICK_DEVICE_ID {icepick_id:#010x}, USER_ID {user_id:#010x}");

        Ok(cc13xx_cc26xx_name(icepick_id, user_id).map(str::to_string))
    }
}

#[cfg(test)]
mod tests {
    use super::cc13xx_cc26xx_name;

    #[test]
    fn cc13xx_cc26xx_names() {
        // Read from a CC2650 and a CC2652R1 LaunchPad.
        assert_eq!(
            cc13xx_cc26xx_name(0x8b99_a02f, 0x8002_f000),
            Some("CC2650F128")
        );
        assert_eq!(
            cc13xx_cc26xx_name(0x3bb4_102f, 0x3002_f000),
            Some("CC2652R")
        );

        // CC1352P: protocol 0xF, CC13 and PA set.
        assert_eq!(
            cc13xx_cc26xx_name(0x3bb4_102f, 0x3282_f000),
            Some("CC1352P")
        );
        // CC2652RB: HPOSC set.
        assert_eq!(
            cc13xx_cc26xx_name(0x3bb4_102f, 0x3102_f000),
            Some("CC2652RB")
        );
        // CC2652PB has no target.
        assert_eq!(cc13xx_cc26xx_name(0x3bb4_102f, 0x3302_f000), None);
        // Unknown family.
        assert_eq!(cc13xx_cc26xx_name(0x0b9b_e02f, 0x3002_f000), None);
    }
}
