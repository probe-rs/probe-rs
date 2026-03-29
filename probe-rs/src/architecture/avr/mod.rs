//! AVR architecture support for UPDI-attached chips via EDBG/nEDBG probes.
//!
//! AVR UPDI chips have no on-chip debug interface — the only operations are
//! flash/erase/read through the programmer. This module provides a minimal
//! [`CoreInterface`] + [`MemoryInterface`] implementation that routes memory
//! operations through the EDBG AVR transport layer.

use crate::{
    CoreInterface, CoreRegister, CoreStatus, CoreType, Error, MemoryInterface,
    core::{
        Architecture, CoreInformation, CoreRegisters, RegisterId, RegisterValue,
        registers::UnwindRule,
    },
    probe::{
        DebugProbe,
        cmsisdap::{
            AvrChipDescriptor, AvrMemoryRegion, CmsisDap, read_attached_pkobn_updi_region,
            write_attached_pkobn_updi_flash,
        },
    },
};
use std::sync::LazyLock;
use std::time::Duration;

/// Minimal core state for AVR — no debug registers, no halt state.
#[derive(Debug, Default)]
pub struct AvrCoreState {
    // Intentionally empty: AVR UPDI has no debug core state to cache.
}

impl AvrCoreState {
    /// Create a new AVR core state.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Placeholder AVR register definitions.
///
/// AVR has no debug-accessible registers through UPDI, but the [`CoreInterface`]
/// trait requires register accessors. We define a minimal set of stubs.
static AVR_PC: CoreRegister = CoreRegister {
    roles: &[crate::RegisterRole::ProgramCounter],
    id: RegisterId(0),
    data_type: crate::RegisterDataType::UnsignedInteger(16),
    unwind_rule: UnwindRule::SpecialRule,
};

static AVR_SP: CoreRegister = CoreRegister {
    roles: &[crate::RegisterRole::StackPointer],
    id: RegisterId(1),
    data_type: crate::RegisterDataType::UnsignedInteger(16),
    unwind_rule: UnwindRule::SpecialRule,
};

static AVR_FP: CoreRegister = CoreRegister {
    roles: &[crate::RegisterRole::FramePointer],
    id: RegisterId(2),
    data_type: crate::RegisterDataType::UnsignedInteger(16),
    unwind_rule: UnwindRule::Preserve,
};

static AVR_RA: CoreRegister = CoreRegister {
    roles: &[crate::RegisterRole::ReturnAddress],
    id: RegisterId(3),
    data_type: crate::RegisterDataType::UnsignedInteger(16),
    unwind_rule: UnwindRule::SpecialRule,
};

/// The minimal AVR register set (placeholder — registers are not accessible via UPDI).
pub static AVR_CORE_REGISTERS: LazyLock<CoreRegisters> =
    LazyLock::new(|| CoreRegisters::new(vec![&AVR_PC, &AVR_SP, &AVR_FP, &AVR_RA]));

/// An AVR "core" that implements memory operations through the EDBG transport.
///
/// This is not a true debug core — halt, step, breakpoints, and register access
/// are all unsupported. The core exists solely to provide a [`MemoryInterface`]
/// so that the standard probe-rs read/write/download paths work without
/// architecture-specific branching in session.rs or memory.rs.
pub struct Avr<'probe> {
    probe: &'probe mut CmsisDap,
    chip: &'static AvrChipDescriptor,
    #[allow(dead_code)]
    state: &'probe mut AvrCoreState,
}

impl<'probe> Avr<'probe> {
    /// Create a new AVR core interface.
    pub fn new(
        probe: &'probe mut CmsisDap,
        state: &'probe mut AvrCoreState,
        chip: &'static AvrChipDescriptor,
    ) -> Self {
        Self { probe, state, chip }
    }

    /// Map an absolute address to an (AvrMemoryRegion, region-relative offset) pair.
    ///
    /// The address space layout uses the chip descriptor's base addresses:
    /// Flash is addressed as 0-based offsets (`[0 .. flash_size)`), but we also accept
    /// the data-space mapping (`[flash_base .. flash_base + flash_size)`) and translate it
    /// back to a 0-based offset automatically.
    ///
    /// - `[0 .. flash_size)` -> Flash (region offset = address)
    /// - `[flash_base .. flash_base + flash_size)` -> Flash (region offset = address - flash_base)
    /// - `[eeprom_base .. eeprom_base + eeprom_size)` -> EEPROM
    /// - `[fuses_base .. fuses_base + fuses_size)` -> Fuses
    /// - `[lock_base .. lock_base + lock_size)` -> Lock
    /// - `[userrow_base .. userrow_base + userrow_size)` -> UserRow
    /// - `[signature_base .. signature_base + prodsig_size)` -> ProdSig
    fn address_to_region(&self, address: u64) -> Result<(AvrMemoryRegion, u32), Error> {
        let addr = u32::try_from(address).map_err(|_| {
            Error::Other(format!("AVR address {address:#010x} exceeds 32-bit range"))
        })?;
        let chip = self.chip;

        if addr < chip.flash_size {
            return Ok((AvrMemoryRegion::Flash, addr));
        }
        if chip.flash_base > 0
            && addr >= chip.flash_base
            && addr < chip.flash_base + chip.flash_size
        {
            return Ok((AvrMemoryRegion::Flash, addr - chip.flash_base));
        }
        if addr >= chip.eeprom_base && addr < chip.eeprom_base + chip.eeprom_size {
            return Ok((AvrMemoryRegion::Eeprom, addr - chip.eeprom_base));
        }
        if addr >= chip.fuses_base && addr < chip.fuses_base + chip.fuses_size {
            return Ok((AvrMemoryRegion::Fuses, addr - chip.fuses_base));
        }
        if addr >= chip.lock_base && addr < chip.lock_base + chip.lock_size {
            return Ok((AvrMemoryRegion::Lock, addr - chip.lock_base));
        }
        if addr >= chip.userrow_base && addr < chip.userrow_base + chip.userrow_size {
            return Ok((AvrMemoryRegion::UserRow, addr - chip.userrow_base));
        }
        if addr >= chip.signature_base && addr < chip.signature_base + chip.prodsig_size {
            return Ok((AvrMemoryRegion::ProdSig, addr - chip.signature_base));
        }

        Err(Error::Other(format!(
            "AVR address {addr:#010x} does not map to any known memory region for {}",
            chip.name
        )))
    }
}

// ---- MemoryInterface (directly on Avr, since we own the probe) ----

impl MemoryInterface for Avr<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        let byte_len = data.len() * 8;
        let mut bytes = vec![0u8; byte_len];
        self.read_8(address, &mut bytes)?;
        for (chunk, word) in bytes.chunks_exact(8).zip(data.iter_mut()) {
            *word = u64::from_le_bytes(chunk.try_into().unwrap());
        }
        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        let byte_len = data.len() * 4;
        let mut bytes = vec![0u8; byte_len];
        self.read_8(address, &mut bytes)?;
        for (chunk, word) in bytes.chunks_exact(4).zip(data.iter_mut()) {
            *word = u32::from_le_bytes(chunk.try_into().unwrap());
        }
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        let byte_len = data.len() * 2;
        let mut bytes = vec![0u8; byte_len];
        self.read_8(address, &mut bytes)?;
        for (chunk, word) in bytes.chunks_exact(2).zip(data.iter_mut()) {
            *word = u16::from_le_bytes(chunk.try_into().unwrap());
        }
        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let (region, offset) = self.address_to_region(address)?;
        let length = u32::try_from(data.len())
            .map_err(|_| Error::Other("AVR read length exceeds 32-bit range".to_string()))?;
        let bytes = read_attached_pkobn_updi_region(self.probe, self.chip, region, offset, length)?;
        if bytes.len() < data.len() {
            return Err(Error::Other(format!(
                "AVR read returned {} bytes, expected {}",
                bytes.len(),
                data.len()
            )));
        }
        data.copy_from_slice(&bytes[..data.len()]);
        Ok(())
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.read_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.write_8(address, &bytes)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.write_8(address, &bytes)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        let bytes: Vec<u8> = data.iter().flat_map(|w| w.to_le_bytes()).collect();
        self.write_8(address, &bytes)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let (region, offset) = self.address_to_region(address)?;
        if region != AvrMemoryRegion::Flash {
            return Err(Error::NotImplemented(
                "AVR writes currently only support the flash region",
            ));
        }
        write_attached_pkobn_updi_flash(self.probe, self.chip, offset, data)?;
        Ok(())
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.write_8(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(true)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

// ---- CoreInterface (stub: most operations are unsupported) ----

impl CoreInterface for Avr<'_> {
    fn wait_for_core_halted(&mut self, _timeout: Duration) -> Result<(), Error> {
        Err(Error::NotImplemented("AVR: halt/debug not supported"))
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        // AVR UPDI has no debug core — report as "halted" so that callers
        // like halted_access() don't attempt to halt the core (which would
        // fail with NotImplemented).
        Ok(true)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        // Report halted so that callers like halted_access() don't try to
        // halt us (which would fail). AVR has no real halt/run distinction
        // through UPDI.
        Ok(CoreStatus::Halted(crate::HaltReason::Request))
    }

    fn halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        Err(Error::NotImplemented("AVR: halt not supported"))
    }

    fn run(&mut self) -> Result<(), Error> {
        // No-op: the core is always running.
        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        // Use probe-level reset (CMSIS-DAP DAP_ResetTarget or nRST toggle).
        self.probe.target_reset().map_err(Error::from)
    }

    fn reset_and_halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        Err(Error::NotImplemented("AVR: reset_and_halt not supported"))
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        Err(Error::NotImplemented("AVR: single-step not supported"))
    }

    fn read_core_reg(&mut self, _address: RegisterId) -> Result<RegisterValue, Error> {
        Err(Error::NotImplemented("AVR: register access not supported"))
    }

    fn write_core_reg(&mut self, _address: RegisterId, _value: RegisterValue) -> Result<(), Error> {
        Err(Error::NotImplemented("AVR: register access not supported"))
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(0)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        Ok(vec![])
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, _unit_index: usize, _addr: u64) -> Result<(), Error> {
        Err(Error::NotImplemented("AVR: breakpoints not supported"))
    }

    fn clear_hw_breakpoint(&mut self, _unit_index: usize) -> Result<(), Error> {
        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &AVR_CORE_REGISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &AVR_PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &AVR_FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &AVR_SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &AVR_RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        false
    }

    fn architecture(&self) -> Architecture {
        Architecture::Avr
    }

    fn core_type(&self) -> CoreType {
        CoreType::Avr
    }

    fn instruction_set(&mut self) -> Result<crate::InstructionSet, Error> {
        Err(Error::NotImplemented("AVR: instruction set query"))
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("AVR: reset catch"))
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("AVR: reset catch"))
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        // No-op: there's nothing to clean up.
        Ok(())
    }
}
