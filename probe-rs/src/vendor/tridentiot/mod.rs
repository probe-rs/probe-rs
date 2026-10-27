//! Trident IoT vendor support.

use crate::Error;
use crate::architecture::arm::{
    ArmChipInfo, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
    dp::{DPIDR, DpAccess, TARGETID},
};
use crate::config::{DebugSequence, Registry};
use crate::vendor::Vendor;
use jep106::JEP106Code;
use probe_rs_target::Chip;

mod sequences;

/// The RT584 family exposes ARM's stock Cortex-M33 ROM table, so detection cannot start from a
/// vendor JEP106 code like other vendors do.
const JEP_ARM: JEP106Code = JEP106Code::new(0x4, 0x3b);
const CORTEX_M33_ROM_PART: u16 = 0x4c9;

/// The RT584 family DPv2 TARGETID reports designer 0x000, which is not a valid JEP106 code, and
/// part number 0x5840. `SOC_CHIP_INFO` is read to confirm the match.
const RT584_TARGETID_DESIGNER: u16 = 0x000;
const RT584_TARGETID_PART: u16 = 0x5840;

/// System Control `SOC_CHIP_INFO` (secure alias), `CHIP_ID` in [31:16].
const SOC_CHIP_INFO: u64 = 0x5000_0000;

/// Trident IoT
#[derive(docsplay::Display)]
pub struct TridentIot;

impl Vendor for TridentIot {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("T32C") {
            return Some(DebugSequence::Arm(sequences::t32cx2x::T32Cx2x::create()));
        }
        None
    }

    fn try_detect_arm_chip(
        &self,
        _registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if chip_info.manufacturer != JEP_ARM || chip_info.part != CORTEX_M33_ROM_PART {
            return Ok(None);
        }

        // Any chip with a stock Cortex-M33 ROM table gets here; a failed read means "not ours".
        match detect_rt584(interface) {
            Ok(name) => Ok(name.map(String::from)),
            Err(error) => {
                tracing::debug!("RT584 detection failed: {error}");
                Ok(None)
            }
        }
    }
}

fn detect_rt584(interface: &mut dyn ArmDebugInterface) -> Result<Option<&'static str>, ArmError> {
    let Some(dp) = interface.current_debug_port() else {
        return Ok(None);
    };

    // TARGETID only exists on DPv2 and later.
    let dpidr: DPIDR = interface.read_dp_register(dp)?;
    if dpidr.version() < 2 {
        return Ok(None);
    }

    let target_id: TARGETID = interface.read_dp_register(dp)?;
    if target_id.tdesigner() != RT584_TARGETID_DESIGNER
        || target_id.tpartno() != RT584_TARGETID_PART
    {
        return Ok(None);
    }

    // TARGETID is shared across the family; CHIP_ID tells the variants apart.
    let chip_info = interface
        .memory_interface(&FullyQualifiedApAddress::v1_with_dp(dp, 0))?
        .read_word_32(SOC_CHIP_INFO)?;

    Ok(match chip_info >> 16 {
        0x0584 => Some("T32CZ20"),
        _ => None,
    })
}
