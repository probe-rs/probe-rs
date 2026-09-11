//! ARM7TDMI debug sequences

use std::sync::Arc;

use crate::architecture::arm7::communication_interface::Arm7tdmiCommunicationInterface;

/// Trait for ARM7TDMI debug sequences
pub trait Arm7tdmiDebugSequence: Send + Sync + std::fmt::Debug {
    /// Initialize the debug interface
    fn debug_core_start(
        &self,
        _interface: &mut Arm7tdmiCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::debug!("ARM7TDMI debug_core_start");
        // Default implementation - can be overridden by specific chips
        Ok(())
    }

    /// Prepare the core for reset
    fn reset_catch_set(
        &self,
        _interface: &mut Arm7tdmiCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::debug!("ARM7TDMI reset_catch_set");
        // Default implementation
        Ok(())
    }

    /// Clear reset catch
    fn reset_catch_clear(
        &self,
        _interface: &mut Arm7tdmiCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::debug!("ARM7TDMI reset_catch_clear");
        // Default implementation
        Ok(())
    }

    /// Called when the debugger detaches from the target. The default implementation resumes
    /// the core if it is currently halted for debugging, leaving the target running normally.
    fn debug_core_stop(
        &self,
        interface: &mut Arm7tdmiCommunicationInterface,
    ) -> Result<(), crate::Error> {
        if interface.is_halted()? {
            interface.resume()?;
        }
        Ok(())
    }

    /// Reset the core
    fn reset_system(
        &self,
        interface: &mut Arm7tdmiCommunicationInterface,
    ) -> Result<(), crate::Error> {
        // Default implementation: pulse the probe's physical reset line. Chips that require a
        // different reset mechanism (e.g. a watchdog-based software reset) should override this.
        interface.reset()?;
        Ok(())
    }

    /// This sequence is called if an image was flashed to RAM directly. It should perform the
    /// necessary preparation to run that image on the core with the ID passed to the function.
    ///
    /// The core should already be `reset_and_halt`ed right before this call.
    fn prepare_running_on_ram(
        &self,
        session: &mut crate::Session,
        vector_table_addr: u64,
        core_id: usize,
    ) -> Result<(), crate::Error> {
        tracing::debug!("RAM flash start for ARM7TDMI core with ID {core_id}");
        let mut core = session.core(core_id)?;
        let pc = core.program_counter().id;
        core.write_core_reg(pc, vector_table_addr)?;
        Ok(())
    }
}

/// Default ARM7TDMI debug sequence
#[derive(Debug, Clone)]
pub struct DefaultArm7tdmiSequence {}

impl DefaultArm7tdmiSequence {
    /// Create a new default ARM7TDMI debug sequence
    pub fn create() -> Arc<dyn Arm7tdmiDebugSequence> {
        Arc::new(Self {})
    }
}

impl Arm7tdmiDebugSequence for DefaultArm7tdmiSequence {}
