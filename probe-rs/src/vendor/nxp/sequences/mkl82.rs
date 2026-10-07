//! Debug sequences for the NXP Kinetis MKL82 family.

use crate::architecture::arm::armv6m::{Aircr, Demcr, Dhcsr};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{
    ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence, DefaultArmSequence,
    cortex_m_core_start, cortex_m_wait_for_reset,
};
use crate::architecture::arm::{ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress};
use crate::core::MemoryMappedRegister;
use crate::session::MissingPermissions;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The sequence handle for the MKL82 family.
#[derive(Debug)]
#[non_exhaustive]
pub struct MKL82;

impl MKL82 {
    /// RCM Force Mode register. The ROM bootloader sets FORCEROM, which
    /// survives system resets and overrides the flash boot configuration.
    const RCM_FM: u64 = 0x4007_F006;

    /// The Kinetis MDM-AP, always accessible even when flash security blocks
    /// the AHB-AP.
    const MDM_AP: u8 = 1;
    const MDM_IDR_EXPECTED: u32 = 0x001C_0020;

    const MDM_STATUS: u64 = 0x00;
    const MDM_CONTROL: u64 = 0x04;
    const MDM_IDR: u64 = 0xFC;

    const MDM_STATUS_MASS_ERASE_ACK: u32 = 1 << 0;
    const MDM_STATUS_FLASH_READY: u32 = 1 << 1;
    const MDM_STATUS_SYSTEM_SECURITY: u32 = 1 << 2;
    /// Reads 0 while the system is held in reset.
    const MDM_STATUS_SYSTEM_RESET_RELEASED: u32 = 1 << 3;
    const MDM_STATUS_MASS_ERASE_ENABLE: u32 = 1 << 5;

    const MDM_CONTROL_MASS_ERASE: u32 = 1 << 0;
    const MDM_CONTROL_DEBUG_REQUEST: u32 = 1 << 2;
    const MDM_CONTROL_SYSTEM_RESET_REQUEST: u32 = 1 << 3;
    const MDM_CONTROL_CORE_HOLD_RESET: u32 = 1 << 4;

    /// Create a sequence handle for the MKL82.
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }

    fn mdm_ap() -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v1_with_default_dp(Self::MDM_AP)
    }

    fn wait_for_mdm_status(
        interface: &mut dyn ArmDebugInterface,
        mask: u32,
        value: u32,
        timeout: Duration,
    ) -> Result<u32, ArmError> {
        let mdm_ap = Self::mdm_ap();
        let start = Instant::now();
        loop {
            match interface.read_raw_ap_register(&mdm_ap, Self::MDM_STATUS) {
                Ok(status) if status & mask == value => return Ok(status),
                Ok(_) => {}
                Err(e) if start.elapsed() >= timeout => return Err(e),
                Err(e) => tracing::trace!("MDM-AP status read failed, retrying: {e}"),
            }
            if start.elapsed() >= timeout {
                return Err(ArmError::Timeout);
            }
        }
    }

    fn write_mdm_control(
        interface: &mut dyn DapAccess,
        value: u32,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let mdm_ap = Self::mdm_ap();
        let start = Instant::now();
        loop {
            // Flush buffered writes so access errors are retried here and
            // cleanup takes effect even if no further reads follow.
            let result = interface
                .write_raw_ap_register(&mdm_ap, Self::MDM_CONTROL, value)
                .and_then(|()| interface.flush());
            match result {
                Ok(()) => return Ok(()),
                Err(e) if start.elapsed() >= timeout => return Err(e),
                Err(e) => tracing::trace!("MDM-AP control write failed, retrying: {e}"),
            }
        }
    }

    /// Run an operation which may assert an MDM-AP control bit, then clear
    /// the register even when the operation fails.
    fn with_mdm_control<T>(
        interface: &mut dyn ArmDebugInterface,
        operation: impl FnOnce(&mut dyn ArmDebugInterface) -> Result<T, ArmError>,
    ) -> Result<T, ArmError> {
        let result = operation(interface);
        let cleanup = Self::write_mdm_control(interface, 0, Duration::from_millis(100));

        match (result, cleanup) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(error), Err(cleanup_error)) => {
                tracing::warn!("Failed to clear the MDM-AP control register: {cleanup_error}");
                Err(error)
            }
        }
    }

    /// Erase and unlock flash through the MDM-AP, including secured devices.
    fn mass_erase(interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let mdm_ap = Self::mdm_ap();

        let idr = interface.read_raw_ap_register(&mdm_ap, Self::MDM_IDR)?;
        if idr != Self::MDM_IDR_EXPECTED {
            return Err(ArmDebugSequenceError::custom(format!(
                "MDM-AP IDR mismatch: expected {:#010x}, got {idr:#010x}",
                Self::MDM_IDR_EXPECTED
            ))
            .into());
        }

        Self::with_mdm_control(interface, |interface| {
            // Hold the system in reset for the duration of the erase so
            // target firmware cannot interfere.
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_SYSTEM_RESET_REQUEST,
                Duration::from_secs(1),
            )?;
            Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_SYSTEM_RESET_RELEASED,
                0,
                Duration::from_secs(1),
            )?;

            // Flash must finish initializing before it accepts an erase request.
            let status = Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_FLASH_READY,
                Self::MDM_STATUS_FLASH_READY,
                Duration::from_secs(1),
            )?;

            if status & Self::MDM_STATUS_MASS_ERASE_ENABLE == 0 {
                return Err(ArmDebugSequenceError::custom(
                    "Mass erase is disabled (FSEC[MEEN]); the device cannot be erased or unlocked via the debug port",
                )
                .into());
            }

            tracing::info!("Requesting mass erase via MDM-AP");
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_SYSTEM_RESET_REQUEST | Self::MDM_CONTROL_MASS_ERASE,
                Duration::from_secs(1),
            )?;

            Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_MASS_ERASE_ACK,
                Self::MDM_STATUS_MASS_ERASE_ACK,
                Duration::from_secs(1),
            )?;

            // The mass erase bit self-clears when the erase has finished.
            let start = Instant::now();
            loop {
                match interface.read_raw_ap_register(&mdm_ap, Self::MDM_CONTROL) {
                    Ok(control) if control & Self::MDM_CONTROL_MASS_ERASE == 0 => break,
                    Ok(_) => {}
                    Err(e) if start.elapsed() >= Duration::from_secs(10) => return Err(e),
                    Err(e) => tracing::trace!("MDM-AP control read failed, retrying: {e}"),
                }
                if start.elapsed() >= Duration::from_secs(10) {
                    return Err(ArmError::Timeout);
                }
            }

            Ok(())
        })?;

        tracing::info!("Mass erase complete");
        Ok(())
    }

    /// Request a debug halt through the MDM-AP. This wakes a core in WAIT or
    /// STOP without resetting it.
    fn halt_via_debug_request(
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        Self::with_mdm_control(interface, |interface| {
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_DEBUG_REQUEST,
                Duration::from_secs(1),
            )?;

            let start = Instant::now();
            loop {
                if let Ok(mut core) = interface.memory_interface(core_ap)
                    && let Ok(dhcsr) = core.read_word_32(Dhcsr::get_mmio_address())
                    && Dhcsr(dhcsr).s_halt()
                {
                    cortex_m_core_start(&mut *core)?;
                    return Ok(());
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
            }
        })
    }

    /// Reset the system through the MDM-AP and halt the core before target
    /// firmware can run.
    fn halt_via_mdm_reset(
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut previous_reset_catch = None;
        let result = Self::with_mdm_control(interface, |interface| {
            // CORE_HOLD_RESET is sampled during reset sequencing.
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_SYSTEM_RESET_REQUEST | Self::MDM_CONTROL_CORE_HOLD_RESET,
                Duration::from_secs(1),
            )?;
            Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_SYSTEM_RESET_RELEASED,
                0,
                Duration::from_secs(1),
            )?;

            // Release the system while keeping the core in reset.
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_CORE_HOLD_RESET,
                Duration::from_secs(1),
            )?;
            let ready = Self::MDM_STATUS_SYSTEM_RESET_RELEASED | Self::MDM_STATUS_FLASH_READY;
            Self::wait_for_mdm_status(interface, ready, ready, Duration::from_secs(1))?;

            // Enable debug and catch the reset vector before releasing the
            // core. C_HALT itself does not persist when CORE_HOLD_RESET is
            // released on this device.
            {
                let mut core = interface.memory_interface(core_ap)?;
                let mut dhcsr = Dhcsr(0);
                dhcsr.set_c_debugen(true);
                dhcsr.enable_write();
                core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

                let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
                // A failed write may still reach the target, so record the
                // original state before attempting to arm the catch.
                previous_reset_catch = Some(demcr.vc_corereset());
                demcr.set_vc_corereset(true);
                core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
            }

            Self::write_mdm_control(interface, 0, Duration::from_secs(1))?;

            let start = Instant::now();
            loop {
                if let Ok(mut core) = interface.memory_interface(core_ap)
                    && let Ok(dhcsr) = core.read_word_32(Dhcsr::get_mmio_address())
                    && Dhcsr(dhcsr).s_halt()
                {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
            }

            Ok(())
        });

        // Preserve a reset catch armed by the caller, including on error.
        let cleanup = if let Some(previous_reset_catch) = previous_reset_catch {
            let start = Instant::now();
            loop {
                let clear_result = (|| {
                    let mut core = interface.memory_interface(core_ap)?;
                    let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
                    demcr.set_vc_corereset(previous_reset_catch);
                    core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
                    core.flush()
                })();
                match clear_result {
                    Ok(()) => break Ok(()),
                    Err(error) if start.elapsed() >= Duration::from_secs(1) => break Err(error),
                    Err(error) => tracing::trace!("Reset-catch cleanup failed, retrying: {error}"),
                }
            }
        } else {
            Ok(())
        };

        match (result, cleanup) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(error), Err(cleanup_error)) => {
                tracing::warn!("Failed to restore reset vector catch: {cleanup_error}");
                return Err(error);
            }
        }

        tracing::info!("Target reset via the MDM-AP: core halted at the reset vector");
        Ok(())
    }

    fn system_reset_via_mdm(interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        Self::with_mdm_control(interface, |interface| {
            Self::write_mdm_control(
                interface,
                Self::MDM_CONTROL_SYSTEM_RESET_REQUEST,
                Duration::from_secs(1),
            )?;
            Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_SYSTEM_RESET_RELEASED,
                0,
                Duration::from_secs(1),
            )?;

            Self::write_mdm_control(interface, 0, Duration::from_secs(1))?;
            Self::wait_for_mdm_status(
                interface,
                Self::MDM_STATUS_SYSTEM_RESET_RELEASED,
                Self::MDM_STATUS_SYSTEM_RESET_RELEASED,
                Duration::from_secs(1),
            )?;
            Ok(())
        })
    }

    fn system_in_reset(interface: &mut dyn ArmDebugInterface) -> bool {
        interface
            .read_raw_ap_register(&Self::mdm_ap(), Self::MDM_STATUS)
            .is_ok_and(|status| status & Self::MDM_STATUS_SYSTEM_RESET_RELEASED == 0)
    }
}

impl ArmDebugSequence for MKL82 {
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // A tight reset loop can leave the AHB-AP briefly accessible between
        // resets, so sample both the MDM-AP reset state and the AHB-AP.
        let mdm_ap = Self::mdm_ap();
        let mut unavailable = false;
        let mut observed_reset = false;
        for _ in 0..8 {
            if let Ok(status) = interface.read_raw_ap_register(&mdm_ap, Self::MDM_STATUS)
                && status & Self::MDM_STATUS_SYSTEM_RESET_RELEASED == 0
            {
                observed_reset = true;
                break;
            }

            let ahb_ok = interface
                .memory_interface(core_ap)
                .and_then(|mut core| core.read_word_32(Dhcsr::get_mmio_address()))
                .is_ok();
            if !ahb_ok {
                unavailable = true;
                break;
            }
        }

        if !unavailable && !observed_reset {
            let mut core = interface.memory_interface(core_ap)?;
            return cortex_m_core_start(&mut *core);
        }

        if !observed_reset {
            match Self::halt_via_debug_request(interface, core_ap) {
                Ok(()) => {
                    tracing::info!("Target halted through the MDM-AP debug request");
                    return Ok(());
                }
                Err(error) => {
                    tracing::debug!("MDM-AP debug request failed: {error}");
                }
            }
        }

        tracing::warn!("Target is unresponsive; resetting and halting it through the MDM-AP");
        Self::halt_via_mdm_reset(interface, core_ap)
    }

    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // The security bit is only meaningful once the flash controller has
        // initialized.
        let status = match Self::wait_for_mdm_status(
            interface,
            Self::MDM_STATUS_FLASH_READY,
            Self::MDM_STATUS_FLASH_READY,
            Duration::from_secs(1),
        ) {
            Ok(status) => status,
            Err(_) if Self::system_in_reset(interface) => {
                // The security state is not yet available. Check it after
                // releasing nRESET in `reset_hardware_deassert`.
                tracing::debug!("System held in reset; deferring the flash security check");
                return Ok(());
            }
            Err(_) => {
                // Try waking the core without resetting its state first.
                if let Err(error) = Self::halt_via_debug_request(interface, default_ap) {
                    tracing::debug!("MDM-AP debug request failed: {error}");
                    Self::halt_via_mdm_reset(interface, default_ap)?;
                }
                Self::wait_for_mdm_status(
                    interface,
                    Self::MDM_STATUS_FLASH_READY,
                    Self::MDM_STATUS_FLASH_READY,
                    Duration::from_secs(1),
                )?
            }
        };

        if status & Self::MDM_STATUS_SYSTEM_SECURITY == 0 {
            return Ok(());
        }

        tracing::warn!(
            "The device is locked (flash security is enabled). A mass erase is required to unlock it."
        );
        permissions
            .erase_all()
            .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

        Self::mass_erase(interface)?;

        let status = Self::wait_for_mdm_status(
            interface,
            Self::MDM_STATUS_FLASH_READY,
            Self::MDM_STATUS_FLASH_READY,
            Duration::from_secs(1),
        )?;
        if status & Self::MDM_STATUS_SYSTEM_SECURITY != 0 {
            return Err(
                ArmDebugSequenceError::custom("Mass erase did not unlock the device").into(),
            );
        }

        Err(ArmError::ReAttachRequired)
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        Some(Self::create())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Restore the flash boot configuration. A failed write should not
        // prevent resetting a sleeping target through the MDM-AP.
        if let Err(e) = interface.write_word_8(Self::RCM_FM, 0) {
            tracing::warn!("Failed to clear RCM_FM before reset: {e}");
        }

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let result = interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .and_then(|()| cortex_m_wait_for_reset(interface));
        if let Err(error) = result {
            // A target can enter STOP before the AHB-AP observes the reset.
            tracing::warn!(
                "SYSRESETREQ could not be observed ({error}); resetting via the MDM-AP instead"
            );
            return Self::system_reset_via_mdm(interface.get_arm_debug_interface()?);
        }

        Ok(())
    }

    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: crate::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        match DefaultArmSequence::create().reset_catch_set(core, core_type, debug_base) {
            // While nRESET is asserted (connect under reset) the debug
            // registers may be unreachable. `reset_hardware_deassert` arms
            // the catch itself once the pin is released.
            Err(e) if Self::system_in_reset(core.get_arm_debug_interface()?) => {
                tracing::debug!("Cannot arm the reset catch while the system is in reset: {e}");
                Ok(())
            }
            result => result,
        }
    }

    fn reset_hardware_deassert(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        DefaultArmSequence::create().reset_hardware_deassert(interface, default_ap)?;

        // Wait for both the reset pin release and flash initialization. Flash
        // ready alone may still reflect the state before nRESET was asserted.
        let ready = Self::MDM_STATUS_SYSTEM_RESET_RELEASED | Self::MDM_STATUS_FLASH_READY;
        let status = Self::wait_for_mdm_status(interface, ready, ready, Duration::from_secs(1))?;
        if status & Self::MDM_STATUS_SYSTEM_SECURITY != 0 {
            return Err(ArmDebugSequenceError::custom(
                "The device is locked (flash security is enabled). Attach without connect-under-reset and with --allow-erase-all to unlock it with a mass erase",
            )
            .into());
        }

        // Debug registers may have been inaccessible under nRESET. Reset
        // through the MDM-AP to enable debug before releasing the core.
        Self::halt_via_mdm_reset(interface, default_ap)
    }
}

impl DebugEraseSequence for MKL82 {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        MKL82::mass_erase(interface)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::arm::dp::{DpAddress, DpRegisterAddress};

    #[derive(Default)]
    struct BufferedMdm {
        control: u32,
        pending: Option<u32>,
        flush_failures: usize,
    }

    impl DapAccess for BufferedMdm {
        fn read_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            _address: DpRegisterAddress,
        ) -> Result<u32, ArmError> {
            unreachable!()
        }

        fn write_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            _address: DpRegisterAddress,
            _value: u32,
        ) -> Result<(), ArmError> {
            unreachable!()
        }

        fn read_raw_ap_register(
            &mut self,
            _ap: &FullyQualifiedApAddress,
            _address: u64,
        ) -> Result<u32, ArmError> {
            unreachable!()
        }

        fn write_raw_ap_register(
            &mut self,
            ap: &FullyQualifiedApAddress,
            address: u64,
            value: u32,
        ) -> Result<(), ArmError> {
            assert_eq!(*ap, MKL82::mdm_ap());
            assert_eq!(address, MKL82::MDM_CONTROL);
            self.pending = Some(value);
            Ok(())
        }

        fn flush(&mut self) -> Result<(), ArmError> {
            let pending = self.pending.take().unwrap();
            if self.flush_failures > 0 {
                self.flush_failures -= 1;
                return Err(ArmError::Timeout);
            }
            self.control = pending;
            Ok(())
        }
    }

    #[test]
    fn mdm_cleanup_flushes_the_reset_release() {
        let mut interface = BufferedMdm {
            control: MKL82::MDM_CONTROL_SYSTEM_RESET_REQUEST,
            ..Default::default()
        };

        MKL82::write_mdm_control(&mut interface, 0, Duration::from_secs(1)).unwrap();

        assert_eq!(interface.control, 0);
        assert_eq!(interface.pending, None);
    }

    #[test]
    fn mdm_control_retries_failed_flushes() {
        let mut interface = BufferedMdm {
            flush_failures: 1,
            ..Default::default()
        };
        let value = MKL82::MDM_CONTROL_SYSTEM_RESET_REQUEST;

        MKL82::write_mdm_control(&mut interface, value, Duration::from_secs(1)).unwrap();

        assert_eq!(interface.control, value);
        assert_eq!(interface.pending, None);
    }

    #[test]
    fn mdm_control_reports_failed_flushes_at_the_deadline() {
        let mut interface = BufferedMdm {
            flush_failures: 1,
            ..Default::default()
        };

        let result = MKL82::write_mdm_control(&mut interface, 0, Duration::ZERO);

        assert!(matches!(result, Err(ArmError::Timeout)));
    }
}
