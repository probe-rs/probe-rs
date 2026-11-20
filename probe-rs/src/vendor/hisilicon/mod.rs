//! HiSilicon vendor support.

use probe_rs_target::Chip;

use crate::{
    config::{DebugSequence, Registry},
    error::Error,
    vendor::Vendor,
};

mod sequences;

/// HiSilicon
#[derive(docsplay::Display)]
pub struct HiSilicon;

impl Vendor for HiSilicon {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        match chip.name.as_str() {
            "HiSilicon 3322" => Some(DebugSequence::Arm(
                sequences::HiSilicon3322Sequence::create(),
            )),
            _ => None,
        }
    }

    fn try_detect_riscv_chip(
        &self,
        _registry: &Registry,
        _probe: &mut crate::architecture::riscv::communication_interface::RiscvCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_sequence_only_for_3322() {
        let family: probe_rs_target::ChipFamily =
            yaml_serde::from_str(include_str!("../../../targets/HiSilicon_3322.yaml")).unwrap();

        let chip = &family.variants[0];
        assert!(HiSilicon.try_create_debug_sequence(chip).is_some());

        let probe_rs_target::CoreAccessOptions::Riscv(options) = &chip.cores[0].core_access_options
        else {
            panic!("3322 core is not RISC-V");
        };
        assert!(matches!(
            options.mem_ap,
            Some(probe_rs_target::ApAddress::V1(1))
        ));
        assert_eq!(options.dm_base, 0);

        let mut other = chip.clone();
        other.name = "HiSilicon WS63".into();
        assert!(HiSilicon.try_create_debug_sequence(&other).is_none());
    }
}
