//! Holtek vendor support (HT32 series)

use jep106::JEP106Code;
use probe_rs_target::Chip;

use crate::{
    architecture::arm::{ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress},
    config::DebugSequence,
    config::Registry,
    error::Error,
    vendor::Vendor,
};

/// Holtek
#[derive(docsplay::Display)]
pub struct Holtek;

const HOLTEK: JEP106Code = JEP106Code { id: 0x76, cc: 0x6 };

impl Vendor for Holtek {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
        // No special debug sequence for Holtek targets for now.
        None
    }

    fn try_detect_arm_chip(
        &self,
        _registry: &Registry,
        interface: &mut dyn ArmDebugInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        // Only attempt detection for Holtek manufacturer
        if chip_info.manufacturer != HOLTEK {
            return Ok(None);
        }

        // Use default AP (v1, AP 0)
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut memory_interface = interface.memory_interface(access_port)?;

        // Holtek HT32F series stores a device ID at 0x40048000 (observed in OpenOCD scripts).
        // Read this register and map to known variants.
        let chip_id = memory_interface.as_mut().read_word_32(0x4004_8000)?;
        tracing::debug!("Holtek: chip id register = {:#010x}", chip_id);

        // Map known IDs to probe-rs target names present in targets YAML.
        // OpenOCD cfg checks for values like 0x52342 / 0x52341 (-> 64KB variants)
        match chip_id {
            0x52342 | 0x52341 => Ok(Some("HT32F52342".to_string())),
            0x52352 | 0x52351 => Ok(Some("HT32F52352".to_string())),
            _ => Ok(None),
        }
    }
}
