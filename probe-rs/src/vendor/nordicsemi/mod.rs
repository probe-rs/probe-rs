//! Nordic Semiconductor vendor support.

use std::collections::{HashMap, hash_map::Entry};

use probe_rs_target::{
    Chip,
    chip_detection::{NordicConfigIdDetection, NordicFicrDetection},
};
use sequences::nrf54l::Nrf54L;

use crate::{
    Error,
    architecture::arm::{
        ArmChipInfo, ArmProbeInterface, FullyQualifiedApAddress, memory::ArmMemoryInterface,
    },
    config::{DebugSequence, Registry},
    vendor::{
        Vendor,
        nordicsemi::sequences::{nrf52::Nrf52, nrf53::Nrf5340, nrf91::Nrf9160},
    },
};

pub mod sequences;

/// Nordic Semiconductor
#[derive(docsplay::Display)]
pub struct NordicSemi;

#[async_trait::async_trait(?Send)]
impl Vendor for NordicSemi {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("nRF5340") {
            DebugSequence::Arm(Nrf5340::create())
        } else if chip.name.starts_with("nRF52") {
            DebugSequence::Arm(Nrf52::create())
        } else if chip.name.starts_with("nRF9160") {
            DebugSequence::Arm(Nrf9160::create())
        } else if chip.name.starts_with("nRF54L") {
            DebugSequence::Arm(Nrf54L::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    async fn try_detect_arm_chip(
        &self,
        registry: &Registry,
        probe: &mut dyn ArmProbeInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if chip_info.manufacturer.get() != Some("Nordic VLSI ASA") {
            return Ok(None);
        }

        // FIXME: This is a bit shaky but good enough for now.
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut memory_interface = probe.memory_interface(access_port).await?;

        // Cache to avoid reading the same register multiple times
        let mut register_values: HashMap<u32, u32> = HashMap::new();

        for family in registry.families() {
            for info in family.chip_detection.iter() {
                let target = if let Some(spec) = info.as_nordic_ficr() {
                    ficr_info_detect(&mut register_values, memory_interface.as_mut(), spec).await
                } else if let Some(spec) = info.as_nordic_configid() {
                    configid_detect(&mut register_values, memory_interface.as_mut(), spec).await
                } else {
                    // Family does not have a Nordic specific detection method
                    continue;
                };

                if target.is_some() {
                    // We have a match
                    return Ok(target);
                }
            }
        }

        Ok(None)
    }
}

async fn ficr_info_detect(
    register_values: &mut HashMap<u32, u32>,
    memory_interface: &mut dyn ArmMemoryInterface,
    spec: &NordicFicrDetection,
) -> Option<String> {
    // Read the PART register, if not already read
    if let Some(part) =
        read_register_cached(register_values, memory_interface, spec.part_address).await
    {
        if part != spec.part {
            return None;
        }

        // Read the VARIANT register, if not already read
        if let Some(variant) =
            read_register_cached(register_values, memory_interface, spec.variant_address).await
        {
            return spec.variants.get(&variant).cloned();
        }
    }

    None
}

async fn configid_detect(
    register_values: &mut HashMap<u32, u32>,
    memory_interface: &mut dyn ArmMemoryInterface,
    spec: &NordicConfigIdDetection,
) -> Option<String> {
    // Read the CONFIGID register, if not already read
    if let Some(configid) =
        read_register_cached(register_values, memory_interface, spec.configid_address).await
    {
        let hwid = configid & 0xFFFF;

        // Match the HWID
        return spec.hwid.get(&hwid).cloned();
    }

    None
}

async fn read_register_cached(
    register_values: &mut HashMap<u32, u32>,
    memory_interface: &mut dyn ArmMemoryInterface,
    address: u32,
) -> Option<u32> {
    match register_values.entry(address) {
        Entry::Occupied(value) => Some(*value.get()),
        Entry::Vacant(e) => {
            if let Ok(value) = memory_interface.read_word_32(address as u64).await {
                e.insert(value);
                Some(value)
            } else {
                None
            }
        }
    }
}
