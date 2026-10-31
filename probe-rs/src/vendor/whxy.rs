//! Wuhan Xinyuan Semiconductor (CMSemicon) CW32 vendor support.
//!
//! Executes the [`Cw32Signature`][probe_rs_target::chip_detection::ChipDetectionMethod]
//! chip detection scheme declared in the target YAML: CW32 parts store an
//! ASCII part-number string in a factory-programmed digital-signature region
//! (ROM_Factory, layout per series in the vendor SDK's `cw32xx_digitalsign.h`),
//! readable through the standard debug port.

use probe_rs_target::{Chip, chip_detection::ChipDetectionMethod};

use crate::{
    Error,
    architecture::arm::{ArmChipInfo, ArmDebugInterface, dp::DpAddress},
    config::{DebugSequence, Registry},
    vendor::Vendor,
};

/// Wuhan Xinyuan
#[derive(Debug)]
pub struct Whxy;

impl std::fmt::Display for Whxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Wuhan Xinyuan")
    }
}

impl Vendor for Whxy {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
        // CW32 parts are plain Cortex-M0+ targets and use the default sequence.
        None
    }

    fn try_detect_arm_chip(
        &self,
        registry: &Registry,
        probe: &mut dyn ArmDebugInterface,
        _chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        for family in registry.families() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_cw32_signature)
            {
                let type_string = match read_type_string(probe, info.type_address) {
                    Ok(s) => s,
                    // Best effort: a failing family must not block the others.
                    Err(_) => continue,
                };
                for (prefix, target) in info.variants.iter() {
                    if type_string.starts_with(prefix.as_str()) {
                        return Ok(Some(target.clone()));
                    }
                }
            }
        }
        Ok(None)
    }
}

/// Reads the ASCII part-number string at `address` from the first access
/// port that answers. Returns an empty string when no port produces a
/// CW32 signature.
///
/// Reading an unmapped address can leave the AP in a sticky-error state,
/// so the debug port is reinitialized after each failing port. Non-CW32
/// devices simply fail all reads here and yield `None` overall.
fn read_type_string(probe: &mut dyn ArmDebugInterface, address: u32) -> Result<String, Error> {
    let aps = probe.access_ports(DpAddress::Default)?;
    for ap in &aps {
        let mut buf = [0u8; 24];
        let read = match probe.memory_interface(ap) {
            Ok(mut mem) => mem.read_8(u64::from(address), &mut buf),
            Err(e) => Err(e),
        };
        match read {
            Ok(()) => {
                if buf.starts_with(b"CW32") {
                    let end = buf
                        .iter()
                        .position(|&b| !(b as char).is_ascii_graphic())
                        .unwrap_or(buf.len());
                    let type_string = String::from_utf8_lossy(&buf[..end]).trim_end().to_string();
                    if type_string.len() >= 6 {
                        return Ok(type_string);
                    }
                }
            }
            Err(_) => {
                let _ = probe.reinitialize();
            }
        }
    }
    Ok(String::new())
}
