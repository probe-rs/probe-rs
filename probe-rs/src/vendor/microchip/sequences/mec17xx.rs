//! Sequences for MEC172x target families

use crate::architecture::arm::armv7m::Demcr;
use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmError, ArmProbeInterface, FullyQualifiedApAddress, armv7m::Dhcsr,
        memory::ArmMemoryInterface, sequences::ArmDebugSequence,
    },
};
use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Marker struct indicating initialization sequencing for Microchip MEC172x family parts.
#[derive(Debug)]
pub struct Mec172x {}

impl Mec172x {
    /// Create the sequencer for the MEC172x family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// Release the CPU core from Reset Extension
    pub async fn release_reset_extension(
        &self,
        memory: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        // Halt the core
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        memory
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())
            .await?;
        memory.flush().await?;

        // Clear VECTOR CATCH and set TRCENA
        let mut demcr: Demcr = memory.read_word_32(Demcr::get_mmio_address()).await?.into();
        demcr.set_trcena(true);
        memory
            .write_word_32(Demcr::get_mmio_address(), demcr.into())
            .await?;
        memory.flush().await?;

        Ok(())
    }

    /// Halt or unhalt the core.
    async fn halt(&self, memory: &mut dyn ArmMemoryInterface, halt: bool) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address()).await?);
        dhcsr.set_c_halt(halt);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        memory
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())
            .await?;
        memory.flush().await?;

        let start = Instant::now();
        let action = if halt { "halt" } else { "unhalt" };

        while Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address()).await?).s_halt() != halt {
            if start.elapsed() > Duration::from_millis(100) {
                tracing::debug!("Exceeded timeout while waiting for the core to {action}");
                return Err(ArmError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    /// Poll the AP's status until it can accept transfers.
    async fn wait_for_enable(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        let mut errors = 0usize;
        let mut disables = 0usize;

        loop {
            match memory.generic_status().await {
                Ok(csw) if csw.DeviceEn => {
                    tracing::debug!(
                        "Device enabled after {}ms with {errors} errors and {disables} invalid statuses",
                        start.elapsed().as_millis()
                    );
                    return Ok(());
                }
                Ok(_) => disables += 1,
                Err(_) => errors += 1,
            }

            if start.elapsed() > timeout {
                tracing::debug!(
                    "Exceeded {}ms timeout while waiting for enable with {errors} errors and {disables} invalid statuses",
                    timeout.as_millis()
                );
                return Err(ArmError::Timeout);
            }

            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for Mec172x {
    async fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap).await?;

        self.release_reset_extension(&mut *core).await
    }

    async fn reset_system(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv8m::Aircr;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        memory
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .await
            .ok();
        memory.flush().await.ok();

        // If all goes well, we lost the debug port. Thanks, boot ROM. Let's bring it back.
        //
        // The ARM communication interface knows how to re-initialize the debug port.
        // Re-initializing the core(s) is on us.
        let ap = memory.fully_qualified_address();
        let interface = memory.get_arm_probe_interface()?;
        interface.reinitialize().await?;

        assert!(debug_base.is_none());
        self.debug_core_start(interface, &ap, core_type, None, None)
            .await?;

        // Are we back?
        self.wait_for_enable(memory, Duration::from_millis(300))
            .await?;

        // We're back. Halt the core so we can establish the reset context.
        self.halt(memory, true).await?;

        Ok(())
    }
}
