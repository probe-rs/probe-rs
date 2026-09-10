//! Types and functions for interacting with target memory.

mod adi_memory_interface;
pub mod romtable;

pub(crate) use adi_memory_interface::ADIMemoryInterface;

use crate::{CoreStatus, memory::MemoryInterface, probe::DebugProbeError};

use super::{ArmDebugInterface, ArmError, FullyQualifiedApAddress};
pub use romtable::{Component, ComponentId, CoresightComponent, PeripheralType, RomTable};

/// One 32-bit access in a batch, for [`ArmMemoryInterface::access_words_32`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access32 {
    /// Read the word at this address.
    Read(u64),

    /// Write this word to this address.
    Write(u64, u32),
}

impl Access32 {
    /// The address this access targets.
    pub fn address(&self) -> u64 {
        match *self {
            Access32::Read(address) => address,
            Access32::Write(address, _) => address,
        }
    }
}

/// Trait for accessing memory behind a memory access port,
/// as defined in the ARM Debug Interface Specification.
pub trait ArmMemoryInterface: MemoryInterface<ArmError> {
    /// Perform 32-bit reads and writes in order, in as few probe transactions as possible.
    ///
    /// Every [`Access32::Read`] writes one word to `values`, in the order the reads appear; writes
    /// contribute nothing. Addresses need not be contiguous or ordered, and every one has to be a
    /// multiple of 4. `values` takes one word per read and has to be at least that long.
    ///
    /// This exists for sequences where a write decides what the next read returns, which is most of
    /// what talking to a debug core consists of: select a register, then read the value it selected.
    /// Issued one at a time, every access costs a round trip to the probe.
    ///
    /// The default performs them one at a time.
    fn access_words_32(
        &mut self,
        accesses: &[Access32],
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let mut read = 0;
        for access in accesses {
            match *access {
                Access32::Read(address) => {
                    values[read] = self.read_word_32(address)?;
                    read += 1;
                }
                Access32::Write(address, value) => self.write_word_32(address, value)?,
            }
        }
        Ok(())
    }

    /// The underlying MemoryAp address.
    fn fully_qualified_address(&self) -> FullyQualifiedApAddress;

    /// The underlying memory AP’s base address.
    fn base_address(&mut self) -> Result<u64, ArmError>;

    /// Get this interface as a [`ArmDebugInterface`] object.
    fn get_arm_debug_interface(&mut self) -> Result<&mut dyn ArmDebugInterface, DebugProbeError>;

    /// Get the current value of the CSW reflected in this probe.
    fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError>;

    /// Inform the probe of the [`CoreStatus`] of the chip/core attached to
    /// the probe.
    //
    // NOTE: this function should be infallible as it is usually only
    // a visual indication.
    fn update_core_status(&mut self, _state: CoreStatus) {}
}
