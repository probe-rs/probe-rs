//! AVR UPDI communication interface trait.
//!
//! Defines the transport-agnostic interface for AVR debug and programming
//! operations. Probe implementations (e.g. CMSIS-DAP) implement this trait
//! to provide AVR support.

use crate::probe::DebugProbeError;
use crate::probe::cmsisdap::{AvrChipDescriptor, AvrDebugState, AvrMemoryRegion};

/// Transport-agnostic interface for AVR UPDI debug and programming operations.
///
/// Implementations encapsulate the probe transport, chip descriptor, and debug
/// session state. Methods do not take chip/state parameters — the implementation
/// manages them internally.
pub trait UpdiInterface: Send {
    // ---- Debug operations ----

    /// Enter OCD debug mode (sign on, attach, set debug session).
    fn enter_debug_mode(&mut self) -> Result<(), DebugProbeError>;

    /// Halt the target and return the PC (byte address).
    fn halt(&mut self) -> Result<u32, DebugProbeError>;

    /// Resume target execution.
    fn run(&mut self) -> Result<(), DebugProbeError>;

    /// Single-step the target and return the new PC (byte address).
    fn step(&mut self) -> Result<u32, DebugProbeError>;

    /// Read the program counter (byte address).
    fn read_pc(&mut self) -> Result<u32, DebugProbeError>;

    /// Query whether the target is halted (`true`) or running (`false`).
    fn status(&mut self) -> Result<bool, DebugProbeError>;

    /// Read the 32 general-purpose registers R0..R31.
    fn read_registers(&mut self) -> Result<[u8; 32], DebugProbeError>;

    /// Read the status register (SREG).
    fn read_sreg(&mut self) -> Result<u8, DebugProbeError>;

    /// Read the stack pointer (16-bit).
    fn read_sp(&mut self) -> Result<u16, DebugProbeError>;

    /// Set a hardware/software breakpoint at the given byte address.
    fn hw_break_set(&mut self, bp_index: u8, address: u32) -> Result<(), DebugProbeError>;

    /// Clear a hardware/software breakpoint.
    fn hw_break_clear(&mut self, bp_index: u8) -> Result<(), DebugProbeError>;

    /// Reset the target via the debug transport.
    fn reset(&mut self) -> Result<(), DebugProbeError>;

    /// Read memory in debug mode using the given memory type and address.
    fn read_memory(
        &mut self,
        memtype: u8,
        address: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError>;

    /// Clean up the OCD debug session (resume, detach, sign off).
    fn cleanup(&mut self) -> Result<(), DebugProbeError>;

    // ---- Programming-mode operations ----

    /// Read a memory region (flash, EEPROM, fuses, etc.) in programming mode.
    fn read_region(
        &mut self,
        region: AvrMemoryRegion,
        offset: u32,
        length: u32,
    ) -> Result<Vec<u8>, DebugProbeError>;

    /// Write flash memory in programming mode.
    fn write_flash(&mut self, offset: u32, data: &[u8]) -> Result<(), DebugProbeError>;

    /// Perform a full chip erase.
    fn erase_chip(&mut self) -> Result<(), DebugProbeError>;

    /// Reset the target via the probe (non-debug reset).
    fn target_reset(&mut self) -> Result<(), DebugProbeError>;

    // ---- State accessors ----

    /// Access the debug session state (read-only).
    fn debug_state(&self) -> &AvrDebugState;

    /// Access the debug session state (mutable).
    fn debug_state_mut(&mut self) -> &mut AvrDebugState;

    /// Access the chip descriptor.
    fn chip(&self) -> &AvrChipDescriptor;
}
