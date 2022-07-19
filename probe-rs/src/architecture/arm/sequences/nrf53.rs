//! Sequences for the nRF53.

use std::sync::Arc;

use super::{nrf::Nrf, ArmDebugSequence};
use crate::architecture::arm::ap::CSW;
use crate::architecture::arm::{
    communication_interface::Initialized, ApAddress, ArmCommunicationInterface, DapAccess,
};
use crate::Memory;

/// The sequence handle for the nRF5340.
pub struct Nrf5340(());

impl Nrf5340 {
    /// Create a new sequence handle for the nRF5340.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl Nrf for Nrf5340 {
    fn core_aps(&self, interface: &mut Memory) -> Vec<(ApAddress, ApAddress)> {
        let ap_address = interface.get_ap();

        let core_aps = [(0, 2), (1, 3)];

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
        ahb_ap_address: ApAddress,
        _ctrl_ap_address: ApAddress,
    ) -> Result<bool, crate::Error> {
        let csw: CSW = arm_interface
            .read_raw_ap_register(ahb_ap_address, 0x00)?
            .into();
        Ok(csw.DeviceEn != 0)
    }

    fn has_network_core(&self) -> bool {
        true
    }
}
