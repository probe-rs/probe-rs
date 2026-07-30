//! RaspberryPi microcontroller support
use jep106::JEP106Code;
use probe_rs_target::Chip;
use sequences::rp235x::Rp235x;
use sequences::rp2040::Rp2040;

use crate::{
    architecture::arm::{
        ApV2Address, ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress, dp::DpAddress,
    },
    config::{DebugSequence, Registry},
    error::Error,
    vendor::Vendor,
};

pub mod sequences;

/// Raspberry Pi
#[derive(docsplay::Display)]
pub struct RaspberryPi;

impl Vendor for RaspberryPi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("RP2040") {
            DebugSequence::Arm(Rp2040::create())
        } else if chip.name.starts_with("RP235") {
            DebugSequence::Arm(Rp235x::create())
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
        const JEP_ARM: JEP106Code = JEP106Code { id: 0x3b, cc: 0x4 };
        const CHIPID_RP2040: u32 = 0x0000_2927;
        const CHIPID_RP235X: u32 = 0x0000_4927;

        // Check for RP2040. We can immediately rule out RP2040 existing if we aren't probing via multidrop.
        if let Some(DpAddress::Multidrop(dp)) = interface.current_debug_port() {
            let ap = FullyQualifiedApAddress::v1_with_dp(DpAddress::Multidrop(dp), 0);
            // Read SYSINFO.CHIP_ID and compare against RP2040 chip_id
            if let Ok(mut memory) = interface.memory_interface(&ap)
                && let Ok(chip_id) = memory.read_word_32(0x4000_0000)
                && (chip_id & 0x0fff_ffff) == CHIPID_RP2040
            {
                return Ok(Some("RP2040".to_string()));
            }
        }

        // Check for RP235X.
        // Before we go poking memory, check that we have a CoreSight Class-1 ROM with a part number of 1225.
        if chip_info.manufacturer != JEP_ARM || chip_info.part != 1225 {
            return Ok(None);
        }

        // Read SYSINFO.CHIP_ID and compare against RP235x chip_id
        let ap = FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(0x2000)));
        if let Ok(mut memory) = interface.memory_interface(&ap)
            && let Ok(chip_id) = memory.read_word_32(0x4000_0000)
            && (chip_id & 0x0fff_ffff) == CHIPID_RP235X
        {
            return Ok(Some("RP235x".to_string()));
        }

        Ok(None)
    }
}
