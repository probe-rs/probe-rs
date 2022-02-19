//! Sequences for the nRF53.

use std::sync::Arc;

use super::ArmDebugSequence;
use crate::architecture::arm::ap::CSW;
use crate::architecture::arm::{
    communication_interface::Initialized, ApAddress, ArmCommunicationInterface, DapAccess,
};

/// The sequence handle for the nRF5340.
pub struct Nrf5340(());

impl Nrf5340 {
    const ERASEALL: u8 = 0x04;
    const ERASEALLSTATUS: u8 = 0x08;

    const APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER: u32 = 0x50005614;
    const RELEASE_FORCEOFF: u32 = 0;

    /// Create a new sequence handle for the nRF5340.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }

    /// Returns true when the core is unlocked and false when it is locked.
    /// The `ap_address` must be of the ahb ap of the core.
    fn is_core_unlocked(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        ap_address: ApAddress,
    ) -> Result<bool, crate::Error> {
        let csw: CSW = arm_interface.read_raw_ap_register(ap_address, 0x00)?.into();
        Ok(csw.DeviceEn != 0)
    }

    /// Unlocks the core by performing an erase all procedure.
    /// The `ap_address` must be of the ctrl ap of the core.
    fn unlock_core(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        ap_address: ApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), crate::Error> {
        permissions.erase_all()?;

        arm_interface.write_raw_ap_register(ap_address, Self::ERASEALL, 1)?;
        while arm_interface.read_raw_ap_register(ap_address, Self::ERASEALLSTATUS)? != 0 {}
        Ok(())
    }

    /// Sets the network core to active running.
    /// The `ap_address` must be of the ahb ap of the application core.
    fn set_network_core_running(&self, interface: &mut crate::Memory) -> Result<(), crate::Error> {
        interface.write_32(
            Self::APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER,
            &[Self::RELEASE_FORCEOFF],
        )?;
        Ok(())
    }
}

impl ArmDebugSequence for Nrf5340 {
    fn debug_device_unlock(
        &self,
        interface: &mut crate::Memory,
        permissions: &crate::Permissions,
    ) -> Result<(), crate::Error> {
        // TODO: Eraseprotect is not considered. If enabled, the debugger must set up the same keys as the firmware does
        // TODO: Approtect and Secure Approtect are not considered. If enabled, the debugger must set up the same keys as the firmware does
        // These keys should be queried from the user if required and once that mechanism is implemented

        let ap_address = interface.get_ap();

        let core_aps = [(0, 2), (1, 3)];

        for (core_ahb_ap, core_ctrl_ap) in core_aps {
            let core_ahb_ap_address = ApAddress {
                ap: core_ahb_ap,
                ..ap_address
            };
            let core_ctrl_ap_address = ApAddress {
                ap: core_ctrl_ap,
                ..ap_address
            };

            log::info!("Checking if core {} is unlocked", core_ahb_ap);
            if self.is_core_unlocked(interface.get_arm_interface()?, core_ahb_ap_address)? {
                log::info!("Core {} is already unlocked", core_ahb_ap);
                continue;
            }

            log::warn!(
                "Core {} is locked. Erase procedure will be started to unlock it.",
                core_ahb_ap
            );
            self.unlock_core(
                interface.get_arm_interface()?,
                core_ctrl_ap_address,
                permissions,
            )?;

            if !self.is_core_unlocked(interface.get_arm_interface()?, core_ahb_ap_address)? {
                return Err(crate::Error::ArchitectureSpecific(
                    format!("Could not unlock core {}", core_ahb_ap).into(),
                ));
            }
        }

        self.set_network_core_running(interface)?;

        Ok(())
    }
}
