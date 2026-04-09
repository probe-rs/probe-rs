//! Sequences for the nRF devices.

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        dp::DpAddress,
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, ArmDebugSequenceError},
    },
    session::MissingPermissions,
};
use std::fmt::Debug;

pub trait Nrf: Sync + Send + Debug {
    /// Returns the ahb_ap and ctrl_ap of every core
    fn core_aps(
        &self,
        dp_address: &DpAddress,
    ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)>;

    /// Returns true when the core is unlocked and false when it is locked.
    fn is_core_unlocked(
        &self,
        interface: &mut dyn ArmDebugInterface,
        ahb_ap_address: &FullyQualifiedApAddress,
        ctrl_ap_address: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError>;

    /// Returns true if a network core is present
    fn has_network_core(&self) -> bool;

    /// Returns true if the chip must be soft-reset after an erase-all operation (ie to unlock APPROTECT).
    ///
    /// Defaults to false. For implementors, make sure to override this method if a reset is required.
    fn requires_soft_reset_after_erase(&self) -> bool {
        false
    }
}

const RESET: u64 = 0x00;
const ERASEALL: u64 = 0x04;
const ERASEALLSTATUS: u64 = 0x08;

const APPLICATION_SPU_PERIPH_PERM: u64 = 0x50003800;

const APPLICATION_RESET_PERIPH_ID: u64 = 5;
const APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER: u32 = 0x50005614;
const APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER: u32 = 0x40005614;
const RELEASE_FORCEOFF: u32 = 0;

/// Performs an erase all operation on the core.
/// The `ap_address` must be of the ctrl ap of the core.
fn erase_all(
    arm_interface: &mut dyn ArmDebugInterface,
    ap_address: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
    reset_after_erase: bool,
) -> Result<(), ArmError> {
    permissions
        .erase_all()
        .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

    arm_interface.write_raw_ap_register(ap_address, ERASEALL, 1)?;

    while arm_interface.read_raw_ap_register(ap_address, ERASEALLSTATUS)? != 0 {}

    if reset_after_erase {
        tracing::debug!("Performing a soft reset after erase operation");

        arm_interface.write_raw_ap_register(ap_address, RESET, 1)?;
        arm_interface.write_raw_ap_register(ap_address, RESET, 0)?;
        std::thread::sleep(std::time::Duration::from_millis(20));

        tracing::debug!("Soft reset complete");
    }

    Ok(())
}

/// Performs an erase all procedure to unlock the core.
/// The `ap_address` must be of the ctrl ap of the core.
fn unlock_core(
    arm_interface: &mut dyn ArmDebugInterface,
    ap_address: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
    reset_after_erase: bool,
) -> Result<(), ArmError> {
    erase_all(arm_interface, ap_address, permissions, reset_after_erase)
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
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let aps = self.core_aps(&default_ap.dp());

        // TODO: Eraseprotect is not considered. If enabled, the debugger must set up the same keys as the firmware does
        // TODO: Approtect and Secure Approtect are not considered. If enabled, the debugger must set up the same keys as the firmware does
        // These keys should be queried from the user if required and once that mechanism is implemented

        for (core_index, (core_ahb_ap_address, core_ctrl_ap_address)) in aps.iter().enumerate() {
            tracing::info!("Checking if core {} is unlocked", core_index);
            if self.is_core_unlocked(interface, core_ahb_ap_address, core_ctrl_ap_address)? {
                tracing::info!("Core {} is already unlocked", core_index);
                continue;
            }

            tracing::warn!(
                "Core {} is locked. Erase procedure will be started to unlock it.",
                core_index
            );
            unlock_core(
                interface,
                core_ctrl_ap_address,
                permissions,
                self.requires_soft_reset_after_erase(),
            )?;

            if !self.is_core_unlocked(interface, core_ahb_ap_address, core_ctrl_ap_address)? {
                tracing::warn!("Core is still locked after erase operation. Retrying");

                unlock_core(
                    interface,
                    core_ctrl_ap_address,
                    permissions,
                    self.requires_soft_reset_after_erase(),
                )?;

                if !self.is_core_unlocked(interface, core_ahb_ap_address, core_ctrl_ap_address)? {
                    return Err(ArmDebugSequenceError::custom(format!(
                        "Could not unlock core {core_index}"
                    ))
                    .into());
                }
            }
        }

        if self.has_network_core() {
            let mut memory_interface = interface.memory_interface(default_ap)?;
            tracing::debug!("Setting network core to running");
            set_network_core_running(&mut *memory_interface)?;

            memory_interface.flush()?;
        }

        Ok(())
    }
}
