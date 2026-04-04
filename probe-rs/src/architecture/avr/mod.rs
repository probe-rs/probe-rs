//! AVR architecture support for UPDI-attached chips via EDBG/nEDBG probes.
//!
//! Provides [`CoreInterface`] + [`MemoryInterface`] that routes operations
//! through the [`UpdiInterface`] trait, including OCD-based debug support
//! (halt, step, breakpoints, register reads).

pub mod communication_interface;
pub use communication_interface::{AvrError, UpdiInterface};

use crate::{
    CoreInterface, CoreRegister, CoreStatus, CoreType, Error, MemoryInterface,
    config::MemoryRegion,
    core::{
        Architecture, CoreInformation, CoreRegisters, RegisterId, RegisterValue,
        registers::UnwindRule,
    },
    probe::cmsisdap::{AvrDebugState, AvrMemoryRegion, DEBUG_MTYPE_EEPROM, DEBUG_MTYPE_SRAM},
};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

/// Core state for AVR, including persistent OCD debug session state.
#[derive(Debug, Default)]
pub struct AvrCoreState {
    /// Debug session state tracked across CoreInterface calls.
    pub debug_state: AvrDebugState,
}

impl AvrCoreState {
    /// Create a new AVR core state.
    pub fn new() -> Self {
        Self::default()
    }
}

// ---- AVR Register Definitions ----
//
// Register IDs (must match GDB's avr-tdep.c built-in layout):
//   0..31  -> R0..R31 (8-bit general purpose)
//   32     -> SREG (status register, 8-bit)
//   33     -> SP (stack pointer, 16-bit)
//   34     -> PC (program counter, 32-bit byte address)
//
// For the CoreInterface trait we need designated PC, SP, FP, and RA registers.
// AVR GCC convention: Y (R28:R29) is the frame pointer. The return address
// lives on the stack, not in a register, so we use a placeholder for RA.

macro_rules! avr_gpr {
    ($name:ident, $id:expr, $label:expr) => {
        static $name: CoreRegister = CoreRegister {
            roles: &[crate::RegisterRole::Core($label)],
            id: RegisterId($id),
            data_type: crate::RegisterDataType::UnsignedInteger(8),
            unwind_rule: UnwindRule::Clear,
        };
    };
}

avr_gpr!(AVR_R0, 0, "R0");
avr_gpr!(AVR_R1, 1, "R1");
avr_gpr!(AVR_R2, 2, "R2");
avr_gpr!(AVR_R3, 3, "R3");
avr_gpr!(AVR_R4, 4, "R4");
avr_gpr!(AVR_R5, 5, "R5");
avr_gpr!(AVR_R6, 6, "R6");
avr_gpr!(AVR_R7, 7, "R7");
avr_gpr!(AVR_R8, 8, "R8");
avr_gpr!(AVR_R9, 9, "R9");
avr_gpr!(AVR_R10, 10, "R10");
avr_gpr!(AVR_R11, 11, "R11");
avr_gpr!(AVR_R12, 12, "R12");
avr_gpr!(AVR_R13, 13, "R13");
avr_gpr!(AVR_R14, 14, "R14");
avr_gpr!(AVR_R15, 15, "R15");
avr_gpr!(AVR_R16, 16, "R16");
avr_gpr!(AVR_R17, 17, "R17");
avr_gpr!(AVR_R18, 18, "R18");
avr_gpr!(AVR_R19, 19, "R19");
avr_gpr!(AVR_R20, 20, "R20");
avr_gpr!(AVR_R21, 21, "R21");
avr_gpr!(AVR_R22, 22, "R22");
avr_gpr!(AVR_R23, 23, "R23");
avr_gpr!(AVR_R24, 24, "R24");
avr_gpr!(AVR_R25, 25, "R25");
avr_gpr!(AVR_R26, 26, "R26");
avr_gpr!(AVR_R27, 27, "R27");

// R28 (Y low) serves as frame pointer in AVR GCC convention
static AVR_R28: CoreRegister = CoreRegister {
    roles: &[
        crate::RegisterRole::Core("R28"),
        crate::RegisterRole::FramePointer,
    ],
    id: RegisterId(28),
    data_type: crate::RegisterDataType::UnsignedInteger(8),
    unwind_rule: UnwindRule::Preserve,
};

avr_gpr!(AVR_R29, 29, "R29");
avr_gpr!(AVR_R30, 30, "R30");
avr_gpr!(AVR_R31, 31, "R31");

// GDB AVR register numbering: r0-r31=0-31, SREG=32, SP=33, PC=34
// This order MUST match GDB's built-in avr-tdep.c layout since GDB ignores
// target-supplied register descriptions for AVR architecture.
static AVR_SREG: CoreRegister = CoreRegister {
    roles: &[
        crate::RegisterRole::Core("SREG"),
        crate::RegisterRole::ProcessorStatus,
    ],
    id: RegisterId(32),
    data_type: crate::RegisterDataType::UnsignedInteger(8),
    unwind_rule: UnwindRule::SpecialRule,
};

static AVR_SP: CoreRegister = CoreRegister {
    roles: &[
        crate::RegisterRole::Core("SP"),
        crate::RegisterRole::StackPointer,
    ],
    id: RegisterId(33),
    data_type: crate::RegisterDataType::UnsignedInteger(16),
    unwind_rule: UnwindRule::SpecialRule,
};

static AVR_PC: CoreRegister = CoreRegister {
    roles: &[
        crate::RegisterRole::Core("PC"),
        crate::RegisterRole::ProgramCounter,
    ],
    id: RegisterId(34),
    data_type: crate::RegisterDataType::UnsignedInteger(32),
    unwind_rule: UnwindRule::SpecialRule,
};

// Return address placeholder — AVR pushes RA onto the stack, there is no
// dedicated RA register. We alias it to R30 (Z low) as a best-effort stand-in.
static AVR_RA: CoreRegister = CoreRegister {
    roles: &[
        crate::RegisterRole::Core("RA"),
        crate::RegisterRole::ReturnAddress,
    ],
    id: RegisterId(30),
    data_type: crate::RegisterDataType::UnsignedInteger(8),
    unwind_rule: UnwindRule::SpecialRule,
};

/// All AVR registers exposed through the debug interface.
pub static AVR_CORE_REGISTERS: LazyLock<CoreRegisters> = LazyLock::new(|| {
    CoreRegisters::new(vec![
        &AVR_R0, &AVR_R1, &AVR_R2, &AVR_R3, &AVR_R4, &AVR_R5, &AVR_R6, &AVR_R7, &AVR_R8, &AVR_R9,
        &AVR_R10, &AVR_R11, &AVR_R12, &AVR_R13, &AVR_R14, &AVR_R15, &AVR_R16, &AVR_R17, &AVR_R18,
        &AVR_R19, &AVR_R20, &AVR_R21, &AVR_R22, &AVR_R23, &AVR_R24, &AVR_R25, &AVR_R26, &AVR_R27,
        &AVR_R28, &AVR_R29, &AVR_R30, &AVR_R31, &AVR_SREG, &AVR_SP, &AVR_PC,
    ])
});

/// An AVR core that implements memory and debug operations through the EDBG transport.
///
/// Supports halt, step, breakpoints, and register reads through the OCD module,
/// as well as flash/erase/read through the programming interface.
pub struct Avr<'probe> {
    interface: &'probe mut dyn UpdiInterface,
    memory_map: &'probe [MemoryRegion],
}

impl<'probe> Avr<'probe> {
    /// Create a new AVR core interface.
    pub fn new(
        interface: &'probe mut dyn UpdiInterface,
        memory_map: &'probe [MemoryRegion],
    ) -> Self {
        Self {
            interface,
            memory_map,
        }
    }

    /// Map an absolute address to an (AvrMemoryRegion, region-relative offset) pair.
    ///
    /// Uses the target's memory_map to determine which region an address belongs to.
    fn address_to_region(&self, address: u64) -> Result<(AvrMemoryRegion, u32), Error> {
        for region in self.memory_map {
            if region.contains(address) {
                let range = region.address_range();
                let offset = (address - range.start) as u32;
                let avr_region = match region {
                    MemoryRegion::Nvm(r) if r.name.as_deref() == Some("Flash") => {
                        AvrMemoryRegion::Flash
                    }
                    MemoryRegion::Nvm(r) if r.name.as_deref() == Some("EEPROM") => {
                        AvrMemoryRegion::Eeprom
                    }
                    MemoryRegion::Generic(r) => match r.name.as_deref() {
                        Some("Fuses") => AvrMemoryRegion::Fuses,
                        Some("Lock") => AvrMemoryRegion::Lock,
                        Some("UserRow") => AvrMemoryRegion::UserRow,
                        Some("ProdSig") => AvrMemoryRegion::ProdSig,
                        _ => {
                            return Err(Error::Avr(AvrError::AddressNotMapped { address }));
                        }
                    },
                    _ => continue,
                };
                return Ok((avr_region, offset));
            }
        }

        Err(Error::Avr(AvrError::AddressNotMapped { address }))
    }

    /// Map an absolute data-space address to a (debug memtype, address) pair for
    /// use when the OCD debug transport is active.
    ///
    /// Uses the target's memory_map to determine the EDBG memory type.
    fn debug_address_to_memtype(&self, address: u64) -> Result<(u8, u32), Error> {
        let addr = u32::try_from(address)
            .map_err(|_| Error::Avr(AvrError::AddressOutOfRange { address }))?;

        // GDB AVR address spaces:
        //   0x000000..          -> Program memory (flash), byte-addressed
        //   0x800000..0x80FFFF  -> Data memory (SRAM/IO/peripherals)
        const GDB_AVR_DATA_OFFSET: u32 = 0x800000;

        if addr >= GDB_AVR_DATA_OFFSET {
            // Data-space address: determine memtype from region
            let data_addr = addr - GDB_AVR_DATA_OFFSET;
            for region in self.memory_map {
                if region.contains(address) {
                    let memtype = match region {
                        MemoryRegion::Nvm(r) if r.name.as_deref() == Some("EEPROM") => {
                            DEBUG_MTYPE_EEPROM
                        }
                        _ => DEBUG_MTYPE_SRAM,
                    };
                    return Ok((memtype, data_addr));
                }
            }
            // Fallback: treat as SRAM
            return Ok((DEBUG_MTYPE_SRAM, data_addr));
        }

        // Program-space address (flash), byte-addressed.
        // Flash is memory-mapped in the data space at flash_base, so read it
        // via SRAM memtype at the data-space address.
        let chip = self.interface.chip();
        if addr >= chip.flash_size {
            return Err(Error::Avr(AvrError::AddressBeyondFlash {
                address: addr,
                flash_size: chip.flash_size,
            }));
        }
        Ok((DEBUG_MTYPE_SRAM, chip.flash_base + addr))
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
        tracing::trace!(
            "AVR read_8: addr=0x{address:08x} len={} debug_mode={}",
            data.len(),
            self.interface.debug_state().in_debug_mode
        );
        if self.interface.debug_state().in_debug_mode {
            // In debug mode, use the debug transport directly
            let (memtype, addr) = self.debug_address_to_memtype(address)?;
            tracing::trace!("AVR read_8: memtype=0x{memtype:02x} mapped_addr=0x{addr:04x}");
            let length = u32::try_from(data.len())
                .map_err(|_| Error::Avr(AvrError::AddressOutOfRange { address }))?;
            let bytes = match self.interface.read_memory(memtype, addr, length) {
                Ok(b) => b,
                Err(e) => {
                    tracing::debug!("AVR read_8: EDBG read failed: {e}");
                    return Err(Error::Avr(AvrError::from(e)));
                }
            };
            if bytes.len() < data.len() {
                return Err(Error::Avr(AvrError::DataLengthMismatch {
                    address,
                    expected: data.len(),
                    actual: bytes.len(),
                }));
            }
            data.copy_from_slice(&bytes[..data.len()]);
            Ok(())
        } else {
            // Programming mode path
            let (region, offset) = self.address_to_region(address)?;
            let length = u32::try_from(data.len())
                .map_err(|_| Error::Avr(AvrError::AddressOutOfRange { address }))?;
            let bytes = self.interface.read_region(region, offset, length)?;
            if bytes.len() < data.len() {
                return Err(Error::Avr(AvrError::DataLengthMismatch {
                    address,
                    expected: data.len(),
                    actual: bytes.len(),
                }));
            }
            data.copy_from_slice(&bytes[..data.len()]);
            Ok(())
        }
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
                "AVR writes only support the flash region; EEPROM/fuse writes require different EDBG commands",
            ));
        }
        self.interface.write_flash(offset, data)?;
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

// ---- CoreInterface (OCD debug support) ----

impl CoreInterface for Avr<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = Instant::now();
        loop {
            match self.interface.status() {
                Ok(true) => return Ok(()),
                Ok(false) => {
                    if start.elapsed() >= timeout {
                        return Err(Error::Timeout);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(Error::Avr(AvrError::from(e))),
            }
        }
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        if !self.interface.debug_state().in_debug_mode {
            // Before OCD is active, report halted to prevent callers from
            // attempting halt/resume operations that would fail. The GDB server
            // checks core_halted() on connect; returning false would trigger a
            // halt attempt before the debug session is established.
            return Ok(true);
        }
        self.interface
            .status()
            .map_err(|e| Error::Avr(AvrError::from(e)))
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        if !self.interface.debug_state().in_debug_mode {
            return Ok(CoreStatus::Unknown);
        }
        let halted = self.interface.status().map_err(AvrError::from)?;
        if halted {
            let reason = if self.interface.debug_state().hw_breakpoint.is_some() {
                crate::HaltReason::Breakpoint(crate::BreakpointCause::Hardware)
            } else {
                crate::HaltReason::Request
            };
            Ok(CoreStatus::Halted(reason))
        } else {
            Ok(CoreStatus::Running)
        }
    }

    fn halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        let pc = self.interface.halt().map_err(AvrError::from)?;
        Ok(CoreInformation { pc: pc as u64 })
    }

    fn run(&mut self) -> Result<(), Error> {
        self.interface
            .run()
            .map_err(|e| Error::Avr(AvrError::from(e)))
    }

    fn reset(&mut self) -> Result<(), Error> {
        if self.interface.debug_state().in_debug_mode {
            self.interface
                .reset()
                .map_err(|e| Error::Avr(AvrError::from(e)))
        } else {
            self.interface
                .target_reset()
                .map_err(|e| Error::Avr(AvrError::from(e)))
        }
    }

    fn reset_and_halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        // Match the dispatch pattern in reset(): use OCD reset when in debug
        // mode, otherwise use the non-OCD target reset.
        if self.interface.debug_state().in_debug_mode {
            self.interface.reset().map_err(AvrError::from)?;
        } else {
            self.interface.target_reset().map_err(AvrError::from)?;
        }
        let pc = self.interface.halt().map_err(AvrError::from)?;
        Ok(CoreInformation { pc: pc as u64 })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        let pc = self.interface.step().map_err(AvrError::from)?;
        Ok(CoreInformation { pc: pc as u64 })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let id = address.0;
        match id {
            0..=31 => {
                let regs = self.interface.read_registers().map_err(AvrError::from)?;
                Ok(RegisterValue::U32(regs[id as usize] as u32))
            }
            32 => {
                let sreg = self.interface.read_sreg().map_err(AvrError::from)?;
                Ok(RegisterValue::U32(sreg as u32))
            }
            33 => {
                let sp = self.interface.read_sp().map_err(AvrError::from)?;
                Ok(RegisterValue::U32(sp as u32))
            }
            34 => {
                let pc = self.interface.read_pc().map_err(AvrError::from)?;
                Ok(RegisterValue::U32(pc))
            }
            _ => Err(Error::Avr(AvrError::UnknownRegister { id })),
        }
    }

    fn write_core_reg(&mut self, _address: RegisterId, _value: RegisterValue) -> Result<(), Error> {
        Err(Error::NotImplemented(
            "AVR: register writes not yet supported",
        ))
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(1)
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        Ok(vec![self.interface.debug_state().hw_breakpoint])
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        if unit_index != 0 {
            return Err(Error::Avr(AvrError::BreakpointUnitOutOfRange {
                index: unit_index,
                max: 0,
            }));
        }
        let addr32 = u32::try_from(addr)
            .map_err(|_| Error::Avr(AvrError::AddressOutOfRange { address: addr }))?;
        self.interface
            .hw_break_set(unit_index as u8, addr32)
            .map_err(AvrError::from)?;
        self.interface.debug_state_mut().hw_breakpoint = Some(addr);
        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        if unit_index != 0 {
            return Err(Error::Avr(AvrError::BreakpointUnitOutOfRange {
                index: unit_index,
                max: 0,
            }));
        }
        self.interface
            .hw_break_clear(unit_index as u8)
            .map_err(AvrError::from)?;
        self.interface.debug_state_mut().hw_breakpoint = None;
        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &AVR_CORE_REGISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &AVR_PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &AVR_R28
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &AVR_SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &AVR_RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.interface.debug_state().in_debug_mode
    }

    fn architecture(&self) -> Architecture {
        Architecture::Avr
    }

    fn core_type(&self) -> CoreType {
        CoreType::Avr
    }

    fn instruction_set(&mut self) -> Result<crate::InstructionSet, Error> {
        Ok(crate::InstructionSet::Avr)
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
        self.interface
            .cleanup()
            .map_err(|e| Error::Avr(AvrError::from(e)))
    }
}
