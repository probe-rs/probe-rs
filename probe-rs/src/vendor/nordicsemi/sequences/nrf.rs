//! Sequences for the nRF devices.

use crate::{
    architecture::arm::{
        ap::MemoryAp,
        communication_interface::Initialized,
        memory::adi_v5_memory_interface::ArmMemoryInterface,
        sequences::{ArmDebugSequence, ArmDebugSequenceError},
        ArmCommunicationInterface, ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress,
    },
    session::MissingPermissions,
};
use std::fmt::Debug;

pub trait Nrf: Sync + Send + Debug {
    /// Returns the ahb_ap and ctrl_ap of every core
    fn core_aps(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)>;

    /// Returns true when the core is unlocked and false when it is locked.
    fn is_core_unlocked(
        &self,
        arm_interface: &mut ArmCommunicationInterface<Initialized>,
        ahb_ap_address: &FullyQualifiedApAddress,
        ctrl_ap_address: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError>;

    /// Returns true if a network core is present
    fn has_network_core(&self) -> bool;
}

const ERASEALL: u8 = 0x04;
const ERASEALLSTATUS: u8 = 0x08;

const APPLICATION_SPU_PERIPH_PERM: u64 = 0x50003800;

const APPLICATION_RESET_PERIPH_ID: u64 = 5;
const APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER: u32 = 0x50005614;
const APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER: u32 = 0x40005614;
const RELEASE_FORCEOFF: u32 = 0;

/// Unlocks the core by performing an erase all procedure.
/// The `ap_address` must be of the ctrl ap of the core.
fn unlock_core(
    arm_interface: &mut ArmCommunicationInterface<Initialized>,
    ap_address: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
) -> Result<(), ArmError> {
    permissions
        .erase_all()
        .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

    arm_interface.write_raw_ap_register(ap_address, ERASEALL, 1)?;

    while arm_interface.read_raw_ap_register(ap_address, ERASEALLSTATUS)? != 0 {}

    Ok(())
}

/// Sets the network core to active running.
fn set_network_core_running(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    // Determine if the RESET peripheral is mapped to secure or non-secure address space.
    let periph_config_address = APPLICATION_SPU_PERIPH_PERM + 0x4 * APPLICATION_RESET_PERIPH_ID;
    let periph_config = interface.read_word_32(periph_config_address)?;
    let is_secure = (periph_config >> 4) & 1 == 1;

    let forceoff_addr = if is_secure {
        tracing::debug!("RESET peripheral is mapped to secure address space");
        APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER
    } else {
        tracing::debug!("RESET peripheral is mapped to non-secure address space");
        APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER
    };

    interface.write_32(forceoff_addr as u64, &[RELEASE_FORCEOFF])?;
    Ok(())
}

impl<T: Nrf> ArmDebugSequence for T {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmProbeInterface,
        default_ap: &MemoryAp,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut interface = interface.memory_interface(default_ap)?;

        // TODO: Eraseprotect is not considered. If enabled, the debugger must set up the same keys as the firmware does
        // TODO: Approtect and Secure Approtect are not considered. If enabled, the debugger must set up the same keys as the firmware does
        // These keys should be queried from the user if required and once that mechanism is implemented

        for (core_index, (core_ahb_ap_address, core_ctrl_ap_address)) in
            self.core_aps(&mut *interface).iter().enumerate()
        {
            tracing::info!("Checking if core {} is unlocked", core_index);
            if self.is_core_unlocked(
                interface.get_arm_communication_interface()?,
                core_ahb_ap_address,
                core_ctrl_ap_address,
            )? {
                tracing::info!("Core {} is already unlocked", core_index);
                continue;
            }

            tracing::warn!(
                "Core {} is locked. Erase procedure will be started to unlock it.",
                core_index
            );
            unlock_core(
                interface.get_arm_communication_interface()?,
                core_ctrl_ap_address,
                permissions,
            )?;

            if !self.is_core_unlocked(
                interface.get_arm_communication_interface()?,
                core_ahb_ap_address,
                core_ctrl_ap_address,
            )? {
                return Err(ArmDebugSequenceError::custom(format!(
                    "Could not unlock core {core_index}"
                ))
                .into());
            }
        }

        if self.has_network_core() {
            tracing::debug!("Setting network core to running");
            set_network_core_running(&mut *interface)?;
        }

        interface.flush()?;

        Ok(())
    }
}
