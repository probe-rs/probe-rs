//! Support for the Vorago VA416xx device family.
use std::{sync::Arc, thread, time::Duration};

use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ap::AccessPort,
        armv7m::Demcr,
        memory::ArmMemoryInterface,
        sequences::{cortex_m_core_start, ArmDebugSequence},
        ArmError, ArmProbeInterface, FullyQualifiedApAddress,
    },
    MemoryMappedRegister,
};

/// Marker structure for the VA416xx device
#[derive(Debug)]
pub struct Va416xx;

impl Va416xx {
    /// Create the sequencer
    pub fn create() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl ArmDebugSequence for Va416xx {
    /// Custom VA416xx core debug start sequence.
    ///
    /// This function performs the regular Cortex-M debug core start sequence in addition to
    /// disabling the ROM protection and the watchdog.
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap)?;
        cortex_m_core_start(&mut *core)?;
        // Disable ROM protection
        core.write_32(0x4001_0010, &[0x000_0001])?;
        // Disable watchdog
        // WDOGLOCK = 0x1ACCE551
        core.write_32(0x400210C0, &[0x1ACCE551])?;
        // WDOGCONTROL = 0x0 (diable)
        core.write_32(0x40021008, &[0])?;
        Ok(())
    }

    /// Resetting the VA416XX breaks the debug connection.
    ///
    /// This custom implementation is similar to the
    /// [crate::vendor::ti::sequences::cc13xx_cc26xx::CC13xxCC26xx::reset_system] implementation
    /// and re-initializes the debug connection after a reset.
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv7m::{Aircr, Dhcsr};
        // Check if the previous code requested a halt before reset
        let demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        // Ignore errors directly after the reset, the debug connection goes down for unknown
        // reasons.
        interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();

        // Since the system went down, including the debug, we should flush any pending operations
        interface.flush().ok();

        // Re-initializing the core(s) is on us.
        let ap = interface.ap();

        let arm_interface = interface.get_arm_communication_interface()?;
        const NUM_RETRIES: u32 = 10;
        for i in 0..NUM_RETRIES {
            match arm_interface.reinitialize() {
                Ok(_) => break,
                Err(e) => {
                    if i == NUM_RETRIES - 1 {
                        return Err(e);
                    }
                    thread::sleep(Duration::from_millis(50));
                }
            }
        }

        assert!(debug_base.is_none());
        self.debug_core_start(arm_interface, ap.ap_address(), core_type, None, None)?;

        if demcr.vc_corereset() {
            // TODO! Find a way to call the armv7m::halt function instead
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();

            const NUM_HALT_RETRIES: u32 = 10;
            for i in 0..NUM_HALT_RETRIES {
                interface.write_word_32(Dhcsr::get_mmio_address(), value.into())?;
                let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
                if dhcsr.s_halt() {
                    break;
                }
                if i >= NUM_HALT_RETRIES - 1 {
                    return Err(ArmError::Timeout);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }

        Ok(())
    }
}
