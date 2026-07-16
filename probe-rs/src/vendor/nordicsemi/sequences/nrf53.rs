//! Sequences for the nRF53.

use std::sync::Arc;

use super::nrf::{Nrf, reset_affects_network, set_network_core_running, wait_for_core_accessible};
use crate::architecture::arm::sequences::ArmDebugSequenceError;
use crate::architecture::arm::{
    ApAddress as ArmApAddress, ArmDebugInterface, ArmError, FullyQualifiedApAddress, ap::CSW,
    dp::DpAddress, memory::ArmMemoryInterface, sequences::ArmDebugSequence,
};
use probe_rs_target::{ApAddress as TargetApAddress, Chip, CoreAccessOptions};

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
                TargetApAddress::V1(0) => (0, 2),
                TargetApAddress::V1(1) => (1, 3),
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

    fn ctrl_ap_for_core(
        &self,
        core_ap: &FullyQualifiedApAddress,
    ) -> Result<Option<FullyQualifiedApAddress>, ArmError> {
        // The nRF5340 application CTRL-AP is documented at AP2:
        // https://docs.nordicsemi.com/bundle/ps_nrf5340/page/ctrl-ap.html
        let ctrl_ap = self
            .core_aps(&core_ap.dp())
            .into_iter()
            .find_map(|(ahb_ap, ctrl_ap)| (ahb_ap == *core_ap).then_some(ctrl_ap))
            .ok_or_else(|| {
                ArmError::from(ArmDebugSequenceError::custom(format!(
                    "No nRF5340 CTRL-AP reset mapping for core access port {:?}",
                    core_ap.ap()
                )))
            })?;

        if core_ap.ap() == &ArmApAddress::V1(0) {
            Ok(Some(ctrl_ap))
        } else {
            Ok(None)
        }
    }

    fn post_reset(&self, interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let core_ap = interface.fully_qualified_address();
        if !reset_affects_network(self, &core_ap) {
            return Ok(());
        }

        // An application reset asserts NETWORK.FORCEOFF. A session using the
        // stock dual-core target owns AP1 as well as AP0, so restore AP1 before
        // returning. Run-control remains core-specific; AP0 reset state does not
        // imply that AP1 should be halted. The application-only target never
        // enters this path.
        let (network_ahb_ap, network_ctrl_ap) = self
            .core_aps(&core_ap.dp())
            .into_iter()
            .find(|(ahb_ap, _)| ahb_ap.ap() == &ArmApAddress::V1(1))
            .ok_or_else(|| {
                ArmError::from(ArmDebugSequenceError::custom(
                    "Dual-core nRF5340 target has no network AP mapping",
                ))
            })?;

        let _ = set_network_core_running(interface)?;
        let arm_interface = interface.get_arm_debug_interface()?;
        if !wait_for_core_accessible(self, arm_interface, &network_ahb_ap, &network_ctrl_ap)? {
            return Err(ArmDebugSequenceError::custom(
                "Network core did not become accessible after application reset",
            )
            .into());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use probe_rs_target::{ApAddress as TargetApAddress, CoreAccessOptions, CoreType};

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
        options.ap = TargetApAddress::V1(1);
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
        options.ap = TargetApAddress::V1(7);

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
        options.ap = TargetApAddress::V1(7);
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

    #[test]
    fn only_dual_target_application_reset_affects_network() {
        let dual = Nrf5340 {
            core_aps: vec![(0, 2), (1, 3)],
        };
        let app_only = Nrf5340 {
            core_aps: vec![(0, 2)],
        };
        let application_ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        let network_ap = FullyQualifiedApAddress::v1_with_default_dp(1);

        assert!(reset_affects_network(&dual, &application_ap));
        assert!(!reset_affects_network(&dual, &network_ap));
        assert!(!reset_affects_network(&app_only, &application_ap));
    }

    #[test]
    fn application_core_resets_through_application_ctrl_ap() {
        let sequence = Nrf5340 {
            core_aps: vec![(0, 2)],
        };
        let core_ap = FullyQualifiedApAddress::v1_with_default_dp(0);

        let ctrl_ap = sequence.ctrl_ap_for_core(&core_ap).unwrap().unwrap();

        assert_eq!(ctrl_ap.ap(), &ArmApAddress::V1(2));
    }

    #[test]
    fn network_core_retains_the_generic_reset_path() {
        let sequence = Nrf5340 {
            core_aps: vec![(0, 2), (1, 3)],
        };
        let core_ap = FullyQualifiedApAddress::v1_with_default_dp(1);

        assert!(sequence.ctrl_ap_for_core(&core_ap).unwrap().is_none());
    }

    #[test]
    fn core_outside_the_selected_target_has_no_reset_mapping() {
        let sequence = Nrf5340 {
            core_aps: vec![(0, 2)],
        };
        let core_ap = FullyQualifiedApAddress::v1_with_default_dp(7);

        assert!(sequence.ctrl_ap_for_core(&core_ap).is_err());
    }
}
