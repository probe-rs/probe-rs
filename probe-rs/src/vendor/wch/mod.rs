//! WCH vendor support.

use probe_rs_target::{
    Chip,
    chip_detection::{ChipDetectionMethod, ObCodeRamSplit},
};

use crate::{
    Error, MemoryInterface,
    config::{DebugSequence, Registry},
    probe::{Probe, wlink::WchLink},
    vendor::Vendor,
};

/// WCH
#[derive(docsplay::Display)]
pub struct Wch;

impl Vendor for Wch {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
        None
    }

    fn try_detect_chip_from_probe(
        &self,
        registry: &Registry,
        probe: &mut Probe,
    ) -> Result<Option<String>, Error> {
        let Some(wlink) = probe.try_into::<WchLink>() else {
            return Ok(None);
        };
        let chip_id = wlink.chip_id();
        if chip_id == 0 {
            return Ok(None);
        }

        // Save the matched detection so we can apply the OB code/RAM split
        // after the borrow on `registry` ends — `read_ob_byte` needs `&mut probe`.
        let mut hit: Option<(String, Option<ObCodeRamSplit>)> = None;
        'outer: for family in registry.families() {
            for method in &family.chip_detection {
                let ChipDetectionMethod::WchLink(detection) = method else {
                    continue;
                };
                let key = chip_id & detection.mask;
                let Some(name) = detection.variants.get(&key) else {
                    continue;
                };
                hit = Some((
                    name.clone(),
                    detection.ob_code_ram_splits.get(&key).cloned(),
                ));
                break 'outer;
            }
        }

        match hit {
            Some((base, Some(split))) => Ok(Some(apply_ob_code_ram_split(probe, &base, &split))),
            Some((base, None)) => Ok(Some(base)),
            None => Ok(None),
        }
    }
}

// OB read failure must not block attach: fall back to the base variant.
fn apply_ob_code_ram_split(probe: &mut Probe, base: &str, split: &ObCodeRamSplit) -> String {
    let Ok(byte) = read_ob_byte(probe, split.address) else {
        return base.to_string();
    };
    split
        .variants
        .get(&(byte & split.mask))
        .cloned()
        .unwrap_or_else(|| base.to_string())
}

fn read_ob_byte(probe: &mut Probe, address: u64) -> Result<u8, Error> {
    let factory = probe.try_get_riscv_interface_builder()?;
    let mut state = factory.create_state();
    let mut interface = factory.attach(&mut state)?;
    interface.enter_debug_mode()?;
    let mut byte = [0u8; 1];
    let result = interface.read_8(address, &mut byte);
    let _ = interface.disable_debug_module();
    result?;
    Ok(byte[0])
}
