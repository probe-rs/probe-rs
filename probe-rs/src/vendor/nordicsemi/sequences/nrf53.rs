//! Sequences for the nRF53.

use std::sync::Arc;

use super::nrf::Nrf;
use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress, ap::CSW, dp::DpAddress,
    sequences::ArmDebugSequence,
};
use probe_rs_target::{ApAddress, Chip, CoreAccessOptions};

/// The sequence handle for the nRF5340.
#[derive(Debug)]
pub struct Nrf5340 {
    core_aps: Vec<(u8, u8)>,
}

impl Nrf5340 {
    /// Create a new sequence handle for the nRF5340.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {
            core_aps: vec![(0, 2), (1, 3)],
        })
    }

    pub(crate) fn create_for_chip(chip: &Chip) -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self::from_chip(chip))
    }

    fn from_chip(chip: &Chip) -> Self {
        let mut core_aps = Vec::with_capacity(chip.cores.len());

        for core in &chip.cores {
            let CoreAccessOptions::Arm(options) = &core.core_access_options else {
                tracing::error!("Unsupported non-Arm core {} in nRF5340 target", core.name);
                return Self {
                    core_aps: Vec::new(),
                };
            };

            let ap_pair = match options.ap {
                ApAddress::V1(0) => (0, 2),
                ApAddress::V1(1) => (1, 3),
                ref ap => {
                    tracing::error!(
                        "Unsupported nRF5340 core access port {ap:?} for core {}",
                        core.name
                    );
                    return Self {
                        core_aps: Vec::new(),
                    };
                }
            };

            if !core_aps.contains(&ap_pair) {
                core_aps.push(ap_pair);
            }
        }

        core_aps.sort_unstable();
        Self { core_aps }
    }
}

impl Nrf for Nrf5340 {
    fn core_aps(
        &self,
        dp_address: &DpAddress,
    ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)> {
        self.core_aps
            .iter()
            .copied()
            .map(|(core_ahb_ap, core_ctrl_ap)| {
                (
                    FullyQualifiedApAddress::v1_with_dp(*dp_address, core_ahb_ap),
                    FullyQualifiedApAddress::v1_with_dp(*dp_address, core_ctrl_ap),
                )
            })
            .collect()
    }

    fn is_core_unlocked(
        &self,
        arm_interface: &mut dyn ArmDebugInterface,
        ahb_ap_address: &FullyQualifiedApAddress,
        _ctrl_ap_address: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let csw: CSW = arm_interface
            .read_raw_ap_register(ahb_ap_address, 0x00)?
            .try_into()?;
        Ok(csw.DeviceEn())
    }

    fn has_network_core(&self) -> bool {
        self.core_aps.iter().any(|&(ahb_ap, _)| ahb_ap == 1)
    }
}

#[cfg(test)]
mod tests {
    use probe_rs_target::{ApAddress, CoreAccessOptions, CoreType};

    use super::*;

    #[cfg(feature = "builtin-targets")]
    use crate::config::Registry;

    #[test]
    fn application_only_target_only_selects_application_access_ports() {
        let chip = Chip::generic_arm("nRF5340_xxAA_APPONLY", CoreType::Armv8m);
        let sequence = Nrf5340::from_chip(&chip);

        assert_eq!(sequence.core_aps, vec![(0, 2)]);
        assert!(!sequence.has_network_core());
    }

    #[test]
    fn dual_core_target_selects_both_access_port_pairs() {
        let mut chip = Chip::generic_arm("nRF5340_xxAA", CoreType::Armv8m);
        let mut network_core = chip.cores[0].clone();
        network_core.name = "network".to_string();
        let CoreAccessOptions::Arm(options) = &mut network_core.core_access_options else {
            unreachable!();
        };
        options.ap = ApAddress::V1(1);
        chip.cores.push(network_core);

        let sequence = Nrf5340::from_chip(&chip);

        assert_eq!(sequence.core_aps, vec![(0, 2), (1, 3)]);
        assert!(sequence.has_network_core());
    }

    #[test]
    fn unsupported_target_does_not_fall_back_to_all_access_ports() {
        let mut chip = Chip::generic_arm("nRF5340_INVALID", CoreType::Armv8m);
        let CoreAccessOptions::Arm(options) = &mut chip.cores[0].core_access_options else {
            unreachable!();
        };
        options.ap = ApAddress::V1(7);

        let sequence = Nrf5340::from_chip(&chip);

        assert!(sequence.core_aps.is_empty());
        assert!(!sequence.has_network_core());
    }

    #[test]
    fn mixed_supported_and_unsupported_target_is_rejected() {
        let mut chip = Chip::generic_arm("nRF5340_INVALID", CoreType::Armv8m);
        let mut unsupported_core = chip.cores[0].clone();
        unsupported_core.name = "unsupported".to_string();
        let CoreAccessOptions::Arm(options) = &mut unsupported_core.core_access_options else {
            unreachable!();
        };
        options.ap = ApAddress::V1(7);
        chip.cores.push(unsupported_core);

        let sequence = Nrf5340::from_chip(&chip);

        assert!(sequence.core_aps.is_empty());
        assert!(!sequence.has_network_core());
    }

    #[cfg(feature = "builtin-targets")]
    #[test]
    fn builtin_targets_select_the_expected_scope() {
        let registry = Registry::from_builtin_families();
        let family = registry
            .families()
            .iter()
            .find(|family| family.name == "nRF53 Series")
            .expect("nRF53 family is built in");

        let stock = family
            .variants
            .iter()
            .find(|chip| chip.name == "nRF5340_xxAA")
            .expect("stock nRF5340 target is built in");
        let stock_sequence = Nrf5340::from_chip(stock);
        assert_eq!(stock_sequence.core_aps, vec![(0, 2), (1, 3)]);
        assert!(stock_sequence.has_network_core());

        let app_only = family
            .variants
            .iter()
            .find(|chip| chip.name == "nRF5340_xxAA_APPONLY")
            .expect("application-only nRF5340 target is built in");
        let app_only_sequence = Nrf5340::from_chip(app_only);
        assert_eq!(app_only_sequence.core_aps, vec![(0, 2)]);
        assert!(!app_only_sequence.has_network_core());
        assert_eq!(app_only.cores.len(), 1);
        assert_eq!(app_only.cores[0].name, "application");
        assert_eq!(
            app_only.flash_algorithms,
            ["nrf53xx_application", "nrf53xx_application_uicr"]
        );
        assert!(
            app_only
                .memory_map
                .iter()
                .all(|region| region.cores() == ["application"])
        );
    }
}
