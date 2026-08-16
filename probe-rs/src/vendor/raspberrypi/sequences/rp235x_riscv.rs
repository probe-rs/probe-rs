//! RISC-V debug sequence for RP235x (Hazard3).

use std::sync::Arc;
use std::time::Duration;

use crate::architecture::riscv::communication_interface::{
    MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface,
};
use crate::architecture::riscv::sequences::RiscvDebugSequence;

/// Debug sequence for RP235x RISC-V (Hazard3) cores.
///
/// Hazard3 has no abstract memory commands and a 2-word program buffer.
/// SBA shares core 1's load/store port, so both harts must be halted and memory
/// access must go through hart 0's program buffer.
#[derive(Debug)]
pub struct Rp235xRiscv {}

impl Rp235xRiscv {
    /// Create a debug sequencer for RP235x RISC-V.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn force_program_buffer(interface: &mut RiscvCommunicationInterface) {
        let config = interface.memory_access_config();
        for width in [RiscvBusAccess::A8, RiscvBusAccess::A16, RiscvBusAccess::A32] {
            config.set_default_method(width, MemoryAccessMethod::ProgramBuffer);
        }
    }

    fn halt_both(
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.set_enabled_harts(0b11);
        let _ = interface
            .select_hart(1)
            .and_then(|_| interface.halt(timeout));
        interface.select_hart(0).map_err(crate::Error::Riscv)?;
        interface.halt(timeout).map_err(crate::Error::Riscv)?;
        Ok(())
    }
}

impl RiscvDebugSequence for Rp235xRiscv {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        interface.clear_abstractauto();
        Self::force_program_buffer(interface);
        // Two Hazard3 harts. `enter_debug_mode` sees hartsellen=1 and skips hart 1.
        if let Err(e) = Self::halt_both(interface, Duration::from_millis(100)) {
            tracing::debug!("RP235x RISC-V halt both harts on connect: {e}");
            let _ = interface.halt(Duration::from_millis(100));
        }
        Self::force_program_buffer(interface);
        Ok(())
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        Self::force_program_buffer(interface);
        interface.set_enabled_harts(0b11);
        interface.select_hart(0).map_err(crate::Error::Riscv)?;
        if let Err(e) = interface.reset_hart_and_halt(timeout) {
            tracing::warn!("RP235x RISC-V hart 0 reset failed: {e}");
        }
        let _ = interface
            .select_hart(1)
            .and_then(|_| interface.reset_hart_and_halt(timeout));

        interface.select_hart(0).map_err(crate::Error::Riscv)?;
        Self::halt_both(interface, timeout)?;
        Self::force_program_buffer(interface);
        Ok(())
    }
}
