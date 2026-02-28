//! Memory-mapped DTM (Debug Transport Module) for RISC-V.
//!
//! When the RISC-V debug module is behind a CoreSight memory access port (for example RP2350),
//! the chip exposes the DMI (Debug Module Interface) registers as offsets into the memory AP's 
//! address space.
//! Each DMI register occupies 4 bytes, so address = register * 4

use crate::architecture::arm::ArmError;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::riscv::communication_interface::RiscvError;
use crate::architecture::riscv::dtm::DtmAccess;
use crate::probe::queue::{DeferredResultIndex, DeferredResultSet};
use crate::probe::{CommandResult, DebugProbeError};
use std::fmt;
use std::time::Duration;

/// DMI operation for the pending queue.
#[derive(Debug)]
enum DmiOp {
    Read(u64),
    Write(u64, u32),
}

/// DTM that performs DMI accesses via a CoreSight memory access port.
pub struct MemApDtm<'state> {
    memory: Box<dyn ArmMemoryInterface + 'state>,
    pending: Vec<(DeferredResultIndex, DmiOp)>,
    results: DeferredResultSet<CommandResult>,
}

impl fmt::Debug for MemApDtm<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemApDtm")
            .field("pending_len", &self.pending.len())
            .finish()
    }
}

fn arm_error_to_riscv(e: ArmError) -> RiscvError {
    match e {
        ArmError::Probe(p) => RiscvError::DebugProbe(p),
        _ => RiscvError::DtmOperationFailed,
    }
}

impl<'state> MemApDtm<'state> {
    /// Creates a DTM that performs DMI accesses via the given memory interface.
    pub fn new(memory: Box<dyn ArmMemoryInterface + 'state>) -> Self {
        Self {
            memory,
            pending: Vec::new(),
            results: DeferredResultSet::new(),
        }
    }

    /// Calculate offset of DMI register.
    fn dmi_register_to_ap_address(&self, dmi_register: u64) -> u64 {
        dmi_register * 4
    }
}

impl DtmAccess for MemApDtm<'_> {
    fn init(&mut self) -> Result<(), RiscvError> {
        // No DTM control register; memory-mapped path is synchronous.
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        // Reset is handled by the session (ARM DAP) on the mem-AP path.
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        Ok(())
    }

    fn clear_error_state(&mut self) -> Result<(), RiscvError> {
        Ok(())
    }

    fn read_deferred_result(
        &mut self,
        index: DeferredResultIndex,
    ) -> Result<CommandResult, RiscvError> {
        // If the result is not ready yet, run pending DMI ops then take it.
        match self.results.take(index.clone()) {
            Ok(result) => Ok(result),
            Err(_) => {
                self.execute()?;
                self.results
                    .take(index)
                    .map_err(|_| RiscvError::BatchedResultNotAvailable)
            }
        }
    }

    fn execute(&mut self) -> Result<(), RiscvError> {
        for (index, op) in std::mem::take(&mut self.pending) {
            match op {
                DmiOp::Read(addr) => {
                    let byte_addr = self.dmi_register_to_ap_address(addr);
                    let value = self
                        .memory
                        .read_word_32(byte_addr)
                        .map_err(arm_error_to_riscv)?;
                    self.results.push(&index, CommandResult::U32(value));
                }
                DmiOp::Write(addr, value) => {
                    let byte_addr = self.dmi_register_to_ap_address(addr);
                    self.memory
                        .write_word_32(byte_addr, value)
                        .map_err(arm_error_to_riscv)?;
                }
            }
        }
        Ok(())
    }

    fn schedule_write(
        &mut self,
        address: u64,
        value: u32,
    ) -> Result<Option<DeferredResultIndex>, RiscvError> {
        let index = DeferredResultIndex::new();
        self.pending
            .push((index.clone(), DmiOp::Write(address, value)));
        Ok(None)
    }

    fn schedule_read(&mut self, address: u64) -> Result<DeferredResultIndex, RiscvError> {
        let index = DeferredResultIndex::new();
        self.pending.push((index.clone(), DmiOp::Read(address)));
        Ok(index)
    }

    fn read_with_timeout(&mut self, address: u64, _timeout: Duration) -> Result<u32, RiscvError> {
        // Memory-mapped path is synchronous; timeout is unused.
        let byte_addr = self.dmi_register_to_ap_address(address);
        self.memory
            .read_word_32(byte_addr)
            .map_err(arm_error_to_riscv)
    }

    fn write_with_timeout(
        &mut self,
        address: u64,
        value: u32,
        _timeout: Duration,
    ) -> Result<Option<u32>, RiscvError> {
        // Memory-mapped path is synchronous; timeout is unused.
        let byte_addr = self.dmi_register_to_ap_address(address);
        self.memory
            .write_word_32(byte_addr, value)
            .map_err(arm_error_to_riscv)?;
        Ok(None)
    }

    fn read_idcode(&mut self) -> Result<Option<u32>, DebugProbeError> {
        // No JTAG = no ID code.
        Ok(None)
    }
}
