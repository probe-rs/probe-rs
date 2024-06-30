//! Sequences for the nRF53.

use std::sync::Arc;

use super::nrf::Nrf;
use crate::architecture::arm::ap::{AccessPort, CSW};
use crate::architecture::arm::memory::adi_v5_memory_interface::ArmProbe;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::{
    communication_interface::Initialized, ArmCommunicationInterface, DapAccess,
    FullyQualifiedApAddress,
};
use crate::architecture::arm::{ApAddress, ArmError};

/// The sequence handle for the nRF5340.
#[derive(Debug)]
pub struct Nrf5340(());

impl Nrf5340 {
    /// Create a new sequence handle for the nRF5340.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl Nrf for Nrf5340 {
    fn core_aps(
        &self,
        memory: &mut dyn ArmProbe,
    ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)> {
        let memory_ap = memory.ap();
        let ap_address = memory_ap.ap_address();

        let core_aps = [(0, 2), (1, 3)];

        core_aps
            .into_iter()
            .map(|(core_ahb_ap, core_ctrl_ap)| {
                (
                    FullyQualifiedApAddress {
                        ap: ApAddress::V1(core_ahb_ap),
                        ..ap_address.clone()
                    },
                    FullyQualifiedApAddress {
                        ap: ApAddress::V1(core_ctrl_ap),
                        ..ap_address.clone()
                    },
                )
            })
            .collect()
    }

    fn is_core_unlocked(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        ahb_ap_address: &FullyQualifiedApAddress,
        _ctrl_ap_address: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let csw: CSW = arm_interface
            .read_raw_ap_register(ahb_ap_address, 0x00)?
            .try_into()?;
        Ok(csw.DeviceEn != 0)
    }

    fn has_network_core(&self) -> bool {
        true
    }
}
