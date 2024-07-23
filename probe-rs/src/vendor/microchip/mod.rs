//! Microchip vendor support.

use probe_rs_target::{chip_detection::ChipDetectionMethod, Chip};

use crate::{
    architecture::arm::{ArmChipInfo, ArmProbeInterface, FullyQualifiedApAddress},
    config::{registry, DebugSequence},
    vendor::{
        microchip::sequences::atsam::{AtSAM, DsuDid},
        Vendor,
    },
    Error,
};

pub mod sequences;

/// Microchip
#[derive(docsplay::Display)]
pub struct Microchip;

impl Vendor for Microchip {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("ATSAMD1")
            || chip.name.starts_with("ATSAMD2")
            || chip.name.starts_with("ATSAMDA")
            || chip.name.starts_with("ATSAMD5")
            || chip.name.starts_with("ATSAME5")
        {
            DebugSequence::Arm(AtSAM::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    fn try_detect_arm_chip(
        &self,
        interface: &mut dyn ArmProbeInterface,
        chip_info: ArmChipInfo,
    ) -> Result<Option<String>, Error> {
        if chip_info.manufacturer.get() != Some("Atmel") || chip_info.part != 0xCD0 {
            return Ok(None);
        }

        // FIXME: This is a bit shaky but good enough for now.
        let access_port = &FullyQualifiedApAddress::v1_with_default_dp(0);
        // This device has an Atmel DSU - Read and parse the DSU DID register
        let did = DsuDid(
            interface
                .memory_interface(access_port)?
                .read_word_32(DsuDid::ADDRESS)?,
        );

        let families = registry::families_ref();
        for family in families.iter() {
            for info in family
                .chip_detection
                .iter()
                .filter_map(ChipDetectionMethod::as_atsam_dsu)
            {
                if info.processor != did.processor() as u8
                    || info.family != did.family() as u8
                    || info.series != did.series() as u8
                {
                    continue;
                }
                for (devsel, variant) in info.variants.iter() {
                    if *devsel == did.devsel() as u8 {
                        return Ok(Some(variant.clone()));
                    }
                }
            }
        }

        Ok(None)
    }
}
