//! Sequences for NXP chips that use ARMv6-M cores.

use crate::architecture::arm::armv6m::{Aircr, Demcr, Dhcsr};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{
    ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence, cortex_m_reset_system,
};
use crate::architecture::arm::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};
use crate::core::MemoryMappedRegister;
use crate::session::MissingPermissions;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The sequence handle for the MKL82 family.
#[derive(Debug)]
pub struct MKL82(());

impl MKL82 {
    /// RCM Force Mode register. The ROM bootloader sets the sticky FORCEROM
    /// field when it runs (for example after a mass erase leaves the flash
    /// option byte blank), and the field survives system resets, so the chip
    /// keeps booting into the ROM instead of the flashed firmware until a
    /// power-on reset.
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
    const MDM_STATUS_MASS_ERASE_ENABLE: u32 = 1 << 5;

    const MDM_CONTROL_MASS_ERASE: u32 = 1 << 0;
    const MDM_CONTROL_SYSTEM_RESET_REQUEST: u32 = 1 << 3;

    /// Create a sequence handle for the MKL82.
    pub fn create() -> Arc<Self> {
        Arc::new(Self(()))
    }

    fn mdm_ap() -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v1_with_default_dp(Self::MDM_AP)
    }

    /// Wait until the given MDM-AP status bits are all set. Read errors are
    /// retried until the deadline: the MDM-AP itself stays accessible while
    /// the system is held in reset, but individual reads can still fail on a
    /// target that is reset-looping.
    fn wait_for_mdm_status(
        interface: &mut dyn ArmDebugInterface,
        mask: u32,
        timeout: Duration,
    ) -> Result<u32, ArmError> {
        let mdm_ap = Self::mdm_ap();
        let start = Instant::now();
        loop {
            match interface.read_raw_ap_register(&mdm_ap, Self::MDM_STATUS) {
                Ok(status) if status & mask == mask => return Ok(status),
                Ok(_) => {}
                Err(e) if start.elapsed() >= timeout => return Err(e),
                Err(e) => tracing::trace!("MDM-AP status read failed, retrying: {e}"),
            }
            if start.elapsed() >= timeout {
                return Err(ArmError::Timeout);
            }
        }
    }

    /// Mass erase the flash via the MDM-AP. This works even when flash
    /// security blocks the AHB-AP, and unlocks a secured device (an erased
    /// KL82 is treated as unsecure while its flash is fully blank).
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

        // Hold the system in reset for the duration of the erase so a
        // reset-looping target cannot interfere.
        interface.write_raw_ap_register(
            &mdm_ap,
            Self::MDM_CONTROL,
            Self::MDM_CONTROL_SYSTEM_RESET_REQUEST,
        )?;

        // The flash controller must have finished initializing before it
        // accepts a mass erase request.
        let status = Self::wait_for_mdm_status(
            interface,
            Self::MDM_STATUS_FLASH_READY,
            Duration::from_secs(1),
        )?;

        if status & Self::MDM_STATUS_MASS_ERASE_ENABLE == 0 {
            // Release the reset request before bailing out.
            interface.write_raw_ap_register(&mdm_ap, Self::MDM_CONTROL, 0)?;
            return Err(ArmDebugSequenceError::custom(
                "Mass erase is disabled (FSEC[MEEN]); the device cannot be erased or unlocked via the debug port",
            )
            .into());
        }

        tracing::info!("Requesting mass erase via MDM-AP");
        interface.write_raw_ap_register(
            &mdm_ap,
            Self::MDM_CONTROL,
            Self::MDM_CONTROL_SYSTEM_RESET_REQUEST | Self::MDM_CONTROL_MASS_ERASE,
        )?;

        Self::wait_for_mdm_status(
            interface,
            Self::MDM_STATUS_MASS_ERASE_ACK,
            Duration::from_secs(1),
        )?;

        // The mass erase bit self-clears when the erase has finished.
        let start = Instant::now();
        loop {
            let control = interface.read_raw_ap_register(&mdm_ap, Self::MDM_CONTROL)?;
            if control & Self::MDM_CONTROL_MASS_ERASE == 0 {
                break;
            }
            if start.elapsed() >= Duration::from_secs(10) {
                return Err(ArmError::Timeout);
            }
        }

        // Release the system reset request.
        interface.write_raw_ap_register(&mdm_ap, Self::MDM_CONTROL, 0)?;

        tracing::info!("Mass erase complete");
        Ok(())
    }
}

impl ArmDebugSequence for MKL82 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        _default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // The security bit is only meaningful once the flash controller has
        // initialized.
        let status = Self::wait_for_mdm_status(
            interface,
            Self::MDM_STATUS_FLASH_READY,
            Duration::from_secs(1),
        )?;

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
        // Clear RCM_FM so the boot source is determined by the flash
        // configuration field again. Ignore errors: if this fails the reset
        // itself may still succeed.
        if let Err(e) = interface.write_word_8(Self::RCM_FM, 0) {
            tracing::warn!("Failed to clear RCM_FM before reset: {e}");
        }

        cortex_m_reset_system(interface)
    }
}

impl DebugEraseSequence for MKL82 {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        MKL82::mass_erase(interface)
    }
}

/// The sequence handle for the LPC80x family.
#[derive(Debug)]
pub struct LPC80x(());

impl LPC80x {
    /// Create a sequence handle for the LPC80x.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }

    // copy-paste of set_hw_breakpoint since we don't have access to core :(
    fn set_hw_breakpoint(
        interface: &mut dyn ArmMemoryInterface,
        bp_register_index: usize,
        addr: u32,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv6m::BpCompx;
        tracing::trace!(
            "Setting breakpoint in lpc804 sequence on address 0x{:08x}",
            addr
        );
        let mut value = BpCompx(0);
        if addr % 4 < 2 {
            // match lower halfword
            value.set_bp_match(0b01);
        } else {
            // match higher halfword
            value.set_bp_match(0b10);
        }
        value.set_comp((addr >> 2) & 0x07FF_FFFF);
        value.set_enable(true);

        let register_addr =
            BpCompx::get_mmio_address() + (bp_register_index * size_of::<u32>()) as u64;
        interface.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    // copy-paste of clear_hw_breakpoint since we don't have access to core :(
    fn clear_hw_breakpoint(
        interface: &mut dyn ArmMemoryInterface,
        bp_unit_index: usize,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv6m::BpCompx;
        tracing::trace!("Clearing breakpoint in lpc804 sequence ");
        let register_addr = BpCompx::get_mmio_address() + (bp_unit_index * size_of::<u32>()) as u64;

        let mut value = BpCompx::from(0);
        value.set_enable(false);

        interface.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    // custom core halt logic from cmsis-pack sequence
    fn force_core_halt(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "force_core_halt");

        let start = Instant::now();
        let mut in_debug_state = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt();
        while start.elapsed() < Duration::from_millis(100) && !in_debug_state {
            in_debug_state = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt();
        }
        // if dhcsr & 0x20000 (s_halt) is still 0 and we hit the above timeout, try halting again.
        if !in_debug_state {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            let start = Instant::now();
            while start.elapsed() < Duration::from_millis(100) {
                if Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
                    break;
                };
            }
        }

        Ok(())
    }
}

impl ArmDebugSequence for LPC80x {
    fn reset_catch_set(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_catch_set");

        // Disable Reset Vector Catch in DEMCR
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        // Map Flash to Vectors
        interface.write_word_32(0x4004_8000, 0x0000_0002)?;

        // Read reset vector from Flash
        let reset_vector = interface.read_word_32(0x0000_0004)?;
        tracing::trace!("Reset Vector is address 0x{:08x}", reset_vector);

        LPC80x::set_hw_breakpoint(interface, 0, reset_vector)?;

        // Clear the status bits by reading from DHCSR
        let _ = interface.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_catch_clear");

        // Disable Reset Vector Catch in DEMCR
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        LPC80x::clear_hw_breakpoint(interface, 0)?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::span!(tracing::Level::TRACE, "reset_system enter");

        // Execute VECTRESET via AIRCR, ignore errors.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let _ = interface.write_32(Aircr::get_mmio_address(), &[aircr.0]);

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(100) {
            // ignore read errors while resetting
            if let Ok(dhcr) = interface.read_word_32(Dhcsr::get_mmio_address())
                && Dhcsr(dhcr).s_halt()
            {
                // return early if we're in debug state
                return Ok(());
            }
        }

        let _ = LPC80x::force_core_halt(interface);

        Ok(())
    }
}
