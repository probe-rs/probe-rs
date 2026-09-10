use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress, armv8m::Aircr, core::armv8m::Dhcsr,
        memory::ArmMemoryInterface, sequences::ArmDebugSequence,
    },
    core::MemoryMappedRegister,
};

/// The sequence handle for the OL23D0 family.
#[derive(Debug)]
pub struct OL23D0(());

impl OL23D0 {
    /// Create a sequence handle for the OL23D0.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for OL23D0 {
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(core_ap)?;
        let memory = memory.as_mut();
        let mut core = OL23D0Core { memory };

        let dhcsr = core.dhcsr()?;
        if dhcsr.s_halt() {
            // Already halted by an earlier session.
        } else if !dhcsr.s_lockup() && core.halt(Duration::from_millis(200)).is_ok() {
            // A running application halts at once, so attaching does not reset it.
        } else {
            // Locked up (as with no valid application), or stuck in the secure bootloader,
            // which sleeps and does not take a halt. Reset and catch it at an application entry.
            core.set_breakpoints()?;
            core.reset(Duration::from_millis(100))?;
        }

        Ok(())
    }

    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = OL23D0Core { memory: core };

        core.set_breakpoints()?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = OL23D0Core { memory: core };

        core.clear_breakpoints()?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = OL23D0Core { memory: interface };

        if core.halt(Duration::from_millis(100)).is_err() {
            // Probable lockup, reset will fix it.
        }

        core.reset(Duration::from_millis(100))?;

        Ok(())
    }
}

/// This copy-pastes & simplifies a part of `arm::core::Armv8m` as we need partial core access,
/// however we cannot construct one here.
struct OL23D0Core<'a> {
    memory: &'a mut dyn ArmMemoryInterface,
}

impl OL23D0Core<'_> {
    /// This is the procedure to get the chip out of lockup. This can happen if there, for example,
    /// is no firmware loaded. Then the first instruction executed will cause a lockup in the
    /// secure region.
    fn set_breakpoints(&mut self) -> Result<(), ArmError> {
        // NOTE: These commands are straight from the manual and are not converted to probe-rs
        // internals as to make it simple to review compared to the manual.

        // To get a clean initial state we manually setup HW breakpoints on the possible
        // application domain entry points. Then we trigger an SCB->AIRCR based reset.
        self.memory.write_word_32(0xE0002008, 0x00200001)?;
        self.memory.write_word_32(0xE000200C, 0x20002001)?;
        self.memory.write_word_32(0xE0002010, 0x20004001)?;
        self.memory.write_word_32(0xE0002014, 0x20005001)?;
        self.memory.write_word_32(0xE0002000, 0x10000083)?;

        Ok(())
    }

    fn clear_breakpoints(&mut self) -> Result<(), ArmError> {
        // Disable breakpoints and clear comparators
        self.memory.write_word_32(0xE0002000, 0x10000082)?;
        self.memory.write_word_32(0xE0002008, 0x0)?;
        self.memory.write_word_32(0xE000200C, 0x0)?;
        self.memory.write_word_32(0xE0002010, 0x0)?;
        self.memory.write_word_32(0xE0002014, 0x0)?;

        Ok(())
    }

    fn reset(&mut self, timeout: Duration) -> Result<(), ArmError> {
        // Trigger a software reset.
        let mut aircr = Aircr(0);
        aircr.set_sysresetreq(true);
        aircr.vectkey();
        self.memory
            .write_word_32(Aircr::get_mmio_address(), aircr.into())?;

        // Give the system time to reset.
        thread::sleep(Duration::from_millis(10));

        // If breakpoints were set, try to wait for them to be halted, else continue.
        self.wait_for_core_halted(timeout).ok();

        // Disable watchdog reset source.
        self.memory.write_word_32(0x4000A834, 0)?;

        Ok(())
    }

    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), ArmError> {
        // Wait until halted state is active again.
        let start = Instant::now();

        loop {
            match self.core_halted() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                // The reset drops the SW-DP until the secure bootloader re-enables debug.
                // Re-establish it and try again.
                Err(_) => self.memory.get_arm_debug_interface()?.reinitialize()?,
            }
            if start.elapsed() >= timeout {
                return Err(ArmError::Timeout);
            }
            // Wait a bit before polling again.
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn dhcsr(&mut self) -> Result<Dhcsr, ArmError> {
        Ok(Dhcsr(self.memory.read_word_32(Dhcsr::get_mmio_address())?))
    }

    fn core_halted(&mut self) -> Result<bool, ArmError> {
        Ok(self.dhcsr()?.s_halt())
    }

    fn halt(&mut self, timeout: Duration) -> Result<(), ArmError> {
        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory
            .write_word_32(Dhcsr::get_mmio_address(), value.into())?;

        self.wait_for_core_halted(timeout)?;

        Ok(())
    }
}
