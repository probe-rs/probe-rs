//! Support for the Vorago VA416xx device family.
use std::sync::Arc;

use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ap::MemoryAp,
        armv7m::Demcr,
        memory::adi_v5_memory_interface::ArmProbe,
        sequences::{cortex_m_core_start, ArmDebugSequence},
        ArmError, ArmProbeInterface,
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
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: MemoryAp,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::info!("vorago specific debug core start");
        let mut core = interface.memory_interface(&core_ap)?;
        cortex_m_core_start(&mut *core)?;
        // Disable ROM protection
        core.write_32(0x4001_0010, &[0x000_0001]).unwrap();
        // Disable watchdog
        // WDOGLOCK = 0x1ACCE551
        core.write_32(0x400210C0, &[0x1ACCE551]).unwrap();
        // WDOGCONTROL = 0x0 (diable)
        core.write_32(0x40021008, &[0]).unwrap();
        Ok(())
    }

    /// Resetting the VA416XX breaks the debug connection.
    ///
    /// This custom implementation is similar to the
    /// [crate::vendor::ti::sequences::cc13xx_cc26xx::CC13xxCC26xx::reset_system] implementation
    /// and re-initializes the debug connection after a reset.
    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv7m::{Aircr, Dhcsr};
        // Check if the previous code requested a halt before reset
        let demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        // Ingore errors directly after the reset, the debug connection goes down for unknown
        // reasons.
        interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();

        // Since the system went down, including the debug, we should flush any pending operations
        interface.flush().ok();

        // Re-initializing the core(s) is on us.
        let ap = interface.ap();
        let arm_interface = interface.get_arm_communication_interface()?;
        arm_interface.reinitialize()?;

        assert!(debug_base.is_none());
        self.debug_core_start(arm_interface, ap, core_type, None, None)?;

        if demcr.vc_corereset() {
            // TODO! Find a way to call the armv7m::halt function instead
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();

            interface.write_word_32(Dhcsr::get_mmio_address(), value.into())?;
        }
        Ok(())
    }
}
