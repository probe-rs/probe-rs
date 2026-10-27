//! Read-only MCXA identification through SYSCON.

use std::collections::HashMap;

use jep106::JEP106Code;
use probe_rs_target::chip_detection::ChipDetectionMethod;

use crate::{
    Error,
    architecture::arm::{ArmChipInfo, ArmDebugInterface, FullyQualifiedApAddress},
    config::Registry,
};

const JEP_NXP: JEP106Code = JEP106Code::new(0, 0x15);

pub(super) fn detect(
    registry: &Registry,
    interface: &mut dyn ArmDebugInterface,
    chip_info: ArmChipInfo,
) -> Result<Option<String>, Error> {
    // MCXA's outer ROM table identifies NXP but reports part 0, not the MCU model.
    if chip_info.manufacturer != JEP_NXP || chip_info.part != 0 {
        return Ok(None);
    }

    let Some(dp) = interface.current_debug_port() else {
        return Ok(None);
    };
    let ap = FullyQualifiedApAddress::v1_with_dp(dp, 0);
    let mut memory = match interface.memory_interface(&ap) {
        Ok(memory) => memory,
        Err(error) => {
            tracing::debug!(?ap, %error, "MCXA detection cannot access memory AP");
            return Ok(None);
        }
    };
    // Detection must not start a mailbox session or reset a target to enable its AP.
    let status = memory.generic_status();
    tracing::debug!(?ap, ?status, "MCXA detection AP status");
    if !matches!(status, Ok(status) if status.DeviceEn()) {
        return Ok(None);
    }
    // Several subfamilies share this register pair. Cache failures too, so another
    // family's metadata does not repeat an inaccessible register read.
    let mut values = HashMap::new();
    let mut read_cached = |address| {
        *values
            .entry(address)
            .or_insert_with(|| match memory.read_word_32(address) {
                Ok(value) => {
                    tracing::debug!("MCXA identity register {address:#010x}: {value:#010x}");
                    Some(value)
                }
                Err(error) => {
                    tracing::debug!("MCXA identity read at {address:#010x} failed: {error}");
                    None
                }
            })
    };

    for family in registry.families() {
        if family.manufacturer != Some(JEP_NXP) {
            continue;
        }
        for method in &family.chip_detection {
            let ChipDetectionMethod::NxpMcxa(spec) = method else {
                continue;
            };
            let address = u64::from(spec.register_address);
            let Some(jtag_id) = read_cached(address) else {
                continue;
            };
            // JTAG_ID[31:28] is the silicon revision, not part of the model ID.
            if jtag_id & 0x0fff_ffff != spec.jtag_id {
                continue;
            }
            let Some(device_type) = read_cached(address + 4) else {
                continue;
            };
            // DEVICE_TYPE[31:16] describes package, pin count and security variant.
            if let Some(name) = spec.variants.get(&(device_type as u16)) {
                return Ok(Some(name.clone()));
            }
        }
    }

    Ok(None)
}
