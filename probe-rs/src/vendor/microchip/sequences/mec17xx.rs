//! Sequences for MEC172x target families

use crate::architecture::arm::armv7m::Demcr;
use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        core::cortex_m::{Vtor, Dhcsr},
        memory::ArmMemoryInterface, sequences::ArmDebugSequence,
    },
};
use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Marker struct indicating initialization sequencing for Microchip MEC172x family of parts
#[derive(Debug)]
pub struct Mec172x {}

impl Mec172x {
    /// Create the sequencer for the MEC172x family of parts
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    /// Halt or unhalt the core
    fn _halt_core(&self, memory: &mut dyn ArmMemoryInterface, halt: bool) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_halt(halt);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        memory.flush()?;

        let start = Instant::now();
        let action = if halt { "halt" } else { "unhalt" };

        while Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?).s_halt() != halt {
            if start.elapsed() > Duration::from_millis(100) {
                tracing::debug!("Exceeded timeout while waiting for the core to {action}");
                return Err(ArmError::Timeout);
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    /// Poll the AP's status until it can accept transfers.
    fn wait_for_enable(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        let mut errors = 0usize;
        let mut disables = 0usize;

        loop {
            match memory.generic_status() {
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

impl ArmDebugSequence for Mec172x {
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(core_ap)?;

        // Halt the core and enable debugging
        let mut dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        memory.flush()?;

        // Enable tracing
        let mut demcr: Demcr = memory.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_trcena(true);
        memory.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        memory.flush()?;

        Ok(())
    }

    fn reset_system(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv8m::Aircr;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        memory
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();
        memory.flush().ok();

        // Re-initialize the Arm debug interface and handle core operations
        let ap = memory.fully_qualified_address();
        let interface = memory.get_arm_debug_interface()?;
        interface.reinitialize()?;

        // Initialize core debugging and tracing
        self.debug_core_start(interface, &ap, core_type, None, None)?;

        // Are we back?
        self.wait_for_enable(memory, Duration::from_millis(300))?;

        // Set the vector table base address to point to the boot ROM's vector table
        memory.write_word_32(Vtor::get_mmio_address(), 0)?;

        // Mask and clear all possible pending external interrupts.  This it to avoid the 
        // problem where the boot ROM starts execution of code in SPI NOR which in turn 
        // enables a timer or starts a DMA transaction which then later triggers an interrupt
        // during the SPI NOR flashing operation.
        //
        const NVIC_BASE: u64 = 0xE000_E100;
        const ICER_OFFSET: u64 = 0x080;
        const ICPR_OFFSET: u64 = 0x180;

        // Iterate over all possible 256 Arm IRQs
        for index in (0..256).step_by(core::mem::size_of::<u32>())
        {
            memory.write_word_32(NVIC_BASE + ICER_OFFSET + index, 0xFFFF_FFFF)?;
            memory.write_word_32(NVIC_BASE + ICPR_OFFSET + index, 0xFFFF_FFFF)?;
        }

        Ok(())
    }
}
