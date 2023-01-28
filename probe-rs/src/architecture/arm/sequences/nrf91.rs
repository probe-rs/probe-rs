//! Sequences for the nRF91.

use std::sync::Arc;

use super::{nrf::Nrf, ArmDebugSequence};
use crate::architecture::arm::ap::AccessPort;
use crate::architecture::arm::memory::adi_v5_memory_interface::ArmProbe;
use crate::architecture::arm::ArmError;
use crate::architecture::arm::{
    communication_interface::Initialized, ApAddress, ArmCommunicationInterface, DapAccess,
};

/// The sequence handle for the nRF9160.
pub struct Nrf9160(());

impl Nrf9160 {
    /// Create a new sequence handle for the nRF9160.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl Nrf for Nrf9160 {
    fn core_aps(&self, memory: &mut dyn ArmProbe) -> Vec<(ApAddress, ApAddress)> {
        let ap_address = memory.ap().ap_address();

        let core_aps = [(0, 4)];

        core_aps
            .into_iter()
            .map(|(core_ahb_ap, core_ctrl_ap)| {
                (
                    ApAddress {
                        ap: core_ahb_ap,
                        ..ap_address
                    },
                    ApAddress {
                        ap: core_ctrl_ap,
                        ..ap_address
                    },
                )
            })
            .collect()
    }

    fn is_core_unlocked(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        _ahb_ap_address: ApAddress,
        ctrl_ap_address: ApAddress,
    ) -> Result<bool, ArmError> {
        let approtect_status = arm_interface.read_raw_ap_register(ctrl_ap_address, 0x00C)?;
        Ok(approtect_status != 0)
    }

    fn has_network_core(&self) -> bool {
        false
    }
}
