//! Sequences for the nRF53.

use std::sync::Arc;

use super::ArmDebugSequence;
use crate::architecture::arm::ap::{MemoryAp, CSW};
use crate::architecture::arm::{
    communication_interface::Initialized, ApAddress, ArmCommunicationInterface, ArmProbeInterface,
    DapAccess,
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

/// The sequence handle for the nRF9160.
pub struct Nrf9160(());

impl Nrf9160 {
    /// Create a new sequence handle for the nRF9160.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl Nrf for Nrf9160 {
    fn core_aps(&self, interface: &mut Memory) -> Vec<(ApAddress, ApAddress)> {
        let ap_address = interface.get_ap();

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
    ) -> Result<bool, crate::Error> {
        let approtect_status = arm_interface.read_raw_ap_register(ctrl_ap_address, 0x00C)?;
        Ok(approtect_status != 0)
    }

    fn has_network_core(&self) -> bool {
        false
    }
}

trait Nrf: Sync + Send {
    /// Returns the ahb_ap and ctrl_ap of every core
    fn core_aps(&self, memory: &mut Memory) -> Vec<(ApAddress, ApAddress)>;

    /// Returns true when the core is unlocked and false when it is locked.
    fn is_core_unlocked(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        ahb_ap_address: ApAddress,
        ctrl_ap_address: ApAddress,
    ) -> Result<bool, crate::Error>;

    /// Returns true if a network core is present
    fn has_network_core(&self) -> bool;
}

const ERASEALL: u8 = 0x04;
const ERASEALLSTATUS: u8 = 0x08;

const APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER: u32 = 0x50005614;
const RELEASE_FORCEOFF: u32 = 0;

/// Unlocks the core by performing an erase all procedure.
/// The `ap_address` must be of the ctrl ap of the core.
fn unlock_core(
    arm_interface: &mut ArmCommunicationInterface<Initialized>,
    ap_address: ApAddress,
    permissions: &crate::Permissions,
) -> Result<(), crate::Error> {
    permissions.erase_all()?;

    arm_interface.write_raw_ap_register(ap_address, ERASEALL, 1)?;
    while arm_interface.read_raw_ap_register(ap_address, ERASEALLSTATUS)? != 0 {}
    Ok(())
}

/// Sets the network core to active running.
fn set_network_core_running(interface: &mut crate::Memory) -> Result<(), crate::Error> {
    interface.write_32(
        APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER as u64,
        &[RELEASE_FORCEOFF],
    )?;
    Ok(())
}

impl<T: Nrf> ArmDebugSequence for T {
    fn debug_device_unlock(
        &self,
        interface: &mut Box<dyn ArmProbeInterface>,
        default_ap: MemoryAp,
        permissions: &crate::Permissions,
    ) -> Result<(), crate::Error> {
        let mut interface = interface.memory_interface(default_ap)?;

        // TODO: Eraseprotect is not considered. If enabled, the debugger must set up the same keys as the firmware does
        // TODO: Approtect and Secure Approtect are not considered. If enabled, the debugger must set up the same keys as the firmware does
        // These keys should be queried from the user if required and once that mechanism is implemented

        for (core_index, (core_ahb_ap_address, core_ctrl_ap_address)) in
            self.core_aps(&mut interface).iter().copied().enumerate()
        {
            log::info!("Checking if core {} is unlocked", core_index);
            if self.is_core_unlocked(
                interface.get_arm_interface()?,
                core_ahb_ap_address,
                core_ctrl_ap_address,
            )? {
                log::info!("Core {} is already unlocked", core_index);
                continue;
            }

            log::warn!(
                "Core {} is locked. Erase procedure will be started to unlock it.",
                core_index
            );
            unlock_core(
                interface.get_arm_interface()?,
                core_ctrl_ap_address,
                permissions,
            )?;

            if !self.is_core_unlocked(
                interface.get_arm_interface()?,
                core_ahb_ap_address,
                core_ctrl_ap_address,
            )? {
                return Err(crate::Error::ArchitectureSpecific(
                    format!("Could not unlock core {}", core_index).into(),
                ));
            }
        }

        if self.has_network_core() {
            set_network_core_running(&mut interface)?;
        }

        Ok(())
    }
}
