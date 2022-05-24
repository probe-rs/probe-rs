//! Register types and the core interface for armv8-a

use crate::architecture::arm::core::armv8a_debug_regs::*;
use crate::architecture::arm::core::register;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::core::{RegisterFile, RegisterValue};
use crate::error::Error;
use crate::memory::{valid_32_address, Memory};
use crate::CoreInterface;
use crate::CoreRegisterAddress;
use crate::CoreStatus;
use crate::DebugProbeError;
use crate::MemoryInterface;
use crate::{Architecture, CoreInformation, CoreType, InstructionSet};
use anyhow::{anyhow, Result};

use super::CortexAState;
use super::ARM_REGISTER_FILE;

use super::instructions::thumb2::{build_ldr, build_mcr, build_mrc, build_str};

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

/// Errors for the ARMv8-A state machine
#[derive(thiserror::Error, Debug)]
pub enum Armv8aError {
    /// Invalid register number
    #[error("Register number {0} is not valid for ARMv8-A")]
    InvalidRegisterNumber(u16),

    /// Not halted
    #[error("Core is running but operation requires it to be halted")]
    NotHalted,

    /// Data Abort occurred
    #[error("A data abort occurred")]
    DataAbort,
}

/// When in 32-bit mode the two words have to be placed in swapped
fn prep_instr_for_itr_32(instruction: u32) -> u32 {
    ((instruction & 0xFFFF) << 16) | ((instruction & 0xFFFF_0000) >> 16)
}

/// Interface for interacting with an ARMv8-A core
pub struct Armv8a<'probe> {
    memory: Memory<'probe>,

    state: &'probe mut CortexAState,

    base_address: u64,

    cti_address: u64,

    sequence: Arc<dyn ArmDebugSequence>,

    num_breakpoints: Option<u32>,
}

impl<'probe> Armv8a<'probe> {
    pub(crate) fn new(
        mut memory: Memory<'probe>,
        state: &'probe mut CortexAState,
        base_address: u64,
        cti_address: u64,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let address = Edscr::get_mmio_address(base_address);
            let edscr = Edscr(memory.read_word_32(address)?);

            log::debug!("State when connecting: {:x?}", edscr);

            let core_state = if edscr.halted() {
                let reason = edscr.halt_reason();

                log::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            state.current_state = core_state;
            state.is_64_bit = edscr.currently_64_bit();
            state.register_cache = vec![None; 17];
            state.initialize();
        }

        Ok(Self {
            memory,
            state,
            base_address,
            cti_address,
            sequence,
            num_breakpoints: None,
        })
    }

    /// Execute an instruction
    fn execute_instruction(&mut self, instruction: u32) -> Result<Edscr, Error> {
        if !self.state.current_state.is_halted() {
            return Err(Error::architecture_specific(Armv8aError::NotHalted));
        }

        let mut final_instruction = instruction;

        if !self.state.is_64_bit {
            // ITR 32-bit instruction encoding requires swapping the half words
            final_instruction = prep_instr_for_itr_32(instruction)
        }

        // Run instruction
        let address = Editr::get_mmio_address(self.base_address);
        self.memory.write_word_32(address, final_instruction)?;

        // Wait for completion
        let address = Edscr::get_mmio_address(self.base_address);
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        while !edscr.ite() {
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Check if we had any aborts, if so clear them and fail
        if edscr.err() || edscr.a() {
            let address = Edrcr::get_mmio_address(self.base_address);
            let mut edrcr = Edrcr(0);
            edrcr.set_cse(true);

            self.memory.write_word_32(address, edrcr.into())?;

            return Err(Error::architecture_specific(Armv8aError::DataAbort));
        }

        Ok(edscr)
    }

    /// Execute an instruction on the CPU and return the result
    fn execute_instruction_with_result(&mut self, instruction: u32) -> Result<u32, Error> {
        // Run instruction
        let mut edscr = self.execute_instruction(instruction)?;

        // Wait for TXfull
        while !edscr.txfull() {
            let address = Edscr::get_mmio_address(self.base_address);
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Read result
        let address = Dbgdtrtx::get_mmio_address(self.base_address);
        let result = self.memory.read_word_32(address)?;

        Ok(result)
    }

    fn execute_instruction_with_input(
        &mut self,
        instruction: u32,
        value: u32,
    ) -> Result<(), Error> {
        // Move value
        let address = Dbgdtrrx::get_mmio_address(self.base_address);
        self.memory.write_word_32(address, value)?;

        // Wait for RXfull
        let address = Edscr::get_mmio_address(self.base_address);
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        while !edscr.rxfull() {
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn reset_register_cache(&mut self) {
        self.state.register_cache = vec![None; 17];
    }

    /// Sync any updated registers back to the core
    fn writeback_registers(&mut self) -> Result<(), Error> {
        for i in 0..self.state.register_cache.len() {
            if let Some((val, writeback)) = self.state.register_cache[i] {
                if writeback {
                    match i {
                        0..=14 => {
                            let instruction = build_mrc(14, 0, i as u16, 0, 5, 0);

                            self.execute_instruction_with_input(instruction, val)?;
                        }
                        15 => {
                            // Move val to r0
                            let instruction = build_mrc(14, 0, 0, 0, 5, 0);

                            self.execute_instruction_with_input(instruction, val)?;

                            // Write to DLR
                            let instruction = build_mrc(15, 3, 0, 4, 5, 1);
                            self.execute_instruction(instruction)?;
                        }
                        _ => {
                            panic!("Logic missing for writeback of register {}", i);
                        }
                    }
                }
            }
        }

        self.reset_register_cache();

        Ok(())
    }

    /// Save register if needed before it gets clobbered by instruction execution
    fn prepare_for_clobber(&mut self, reg: u16) -> Result<(), Error> {
        if self.state.register_cache[reg as usize].is_none() {
            // TODO 64-bit - handle non-32 bit values
            // cache reg since we're going to clobber it
            let val: u32 = self.read_core_reg(CoreRegisterAddress(reg))?.try_into()?;

            // Mark reg as needing writeback
            self.state.register_cache[reg as usize] = Some((val, true));
        }

        Ok(())
    }

    fn set_reg_value(&mut self, reg: u16, value: u32) -> Result<(), Error> {
        let instruction = build_mrc(14, 0, reg, 0, 5, 0);

        self.execute_instruction_with_input(instruction, value)
    }

    fn ack_cti_halt(&mut self) -> Result<(), Error> {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);

        let address = CtiIntack::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, ack.into())?;

        loop {
            let address = CtiTrigoutstatus::get_mmio_address(self.cti_address);
            let trig_status = CtiTrigoutstatus(self.memory.read_word_32(address)?);

            if trig_status.status(0) == 0 {
                break;
            }
        }

        Ok(())
    }
}

impl<'probe> CoreInterface for Armv8a<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        let address = Edscr::get_mmio_address(self.base_address);

        while start.elapsed() < timeout {
            let edscr = Edscr(self.memory.read_word_32(address)?);
            if edscr.halted() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        let address = Edscr::get_mmio_address(self.base_address);
        let edscr = Edscr(self.memory.read_word_32(address)?);

        Ok(edscr.halted())
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        // Ungate halt CTI channel
        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(0, 1);

        let address = CtiGate::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, cti_gate.into())?;

        // Pulse it
        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(0, 1);

        let address = CtiApppulse::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, pulse.into())?;

        // Wait for halt
        self.wait_for_core_halted(timeout)?;

        // Reset our cached values
        self.reset_register_cache();

        // Update core status
        let _ = self.status()?;

        // Gate halt channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, cti_gate.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn run(&mut self) -> Result<(), Error> {
        // set writeback values
        self.writeback_registers()?;

        self.ack_cti_halt()?;

        // Ungate restart CTI channel
        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(1, 1);

        let address = CtiGate::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, cti_gate.into())?;

        // Pulse it
        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(1, 1);

        let address = CtiApppulse::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, pulse.into())?;

        // Wait for ack
        let address = Edprsr::get_mmio_address(self.base_address);

        loop {
            let edprsr = Edprsr(self.memory.read_word_32(address)?);
            if edprsr.sdr() {
                break;
            }
        }

        // Recompute / verify current state
        self.state.current_state = CoreStatus::Running;
        let _ = self.status()?;

        // Gate restart channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address(self.cti_address);
        self.memory.write_word_32(address, cti_gate.into())?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(
            &mut self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        // Reset our cached values
        self.reset_register_cache();

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_catch_set(
            &mut self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;
        self.sequence.reset_system(
            &mut self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        // Release from reset
        self.sequence.reset_catch_clear(
            &mut self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        self.wait_for_core_halted(timeout)?;

        // Update core status
        let _ = self.status()?;

        // Reset our cached values
        self.reset_register_cache();

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // Load EDECR, set SS bit for step mode
        let edecr_address = Edecr::get_mmio_address(self.base_address);
        let mut edecr = Edecr(self.memory.read_word_32(edecr_address)?);

        edecr.set_ss(true);
        self.memory.write_word_32(edecr_address, edecr.into())?;

        // Resume
        self.run()?;

        // Wait for halt
        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Reset EDECR
        edecr.set_ss(false);
        self.memory.write_word_32(edecr_address, edecr.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<RegisterValue, Error> {
        let reg_num = address.0;

        // check cache
        if (reg_num as usize) < self.state.register_cache.len() {
            if let Some(cached_result) = self.state.register_cache[reg_num as usize] {
                return Ok(cached_result.0.into());
            }
        }

        // TODO 64-bit - update with support
        if self.state.is_64_bit {
            return Err(Error::Other(anyhow!("64-bit not currently supported")));
        }

        // Generate instruction to extract register
        let result = match reg_num {
            0..=14 => {
                // r0-r14, valid
                // MCR p14, 0, <Rd>, c0, c5, 0 ; Write DBGDTRTXint Register
                let instruction = build_mcr(14, 0, reg_num, 0, 5, 0);

                self.execute_instruction_with_result(instruction)
            }
            15 => {
                // PC, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC p15, 3, r0, c4, c5, 1 ; Read DLR to r0
                let instruction = build_mrc(15, 3, 0, 4, 5, 1);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let pc = self.execute_instruction_with_result(instruction)?;

                Ok(pc)
            }
            16 => {
                // CPSR, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC c15, 3, r0, c4, c5, 0
                let instruction = build_mrc(15, 3, 0, 4, 5, 0);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let cpsr = self.execute_instruction_with_result(instruction)?;

                Ok(cpsr)
            }
            _ => Err(Error::architecture_specific(
                Armv8aError::InvalidRegisterNumber(reg_num),
            )),
        };

        if let Ok(value) = result {
            self.state.register_cache[reg_num as usize] = Some((value, false));

            Ok(value.into())
        } else {
            Err(result.err().unwrap())
        }
    }

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: RegisterValue) -> Result<()> {
        // TODO 64-bit
        let value: u32 = value.try_into()?;
        let reg_num = address.0;

        if (reg_num as usize) >= self.state.register_cache.len() {
            return Err(
                Error::architecture_specific(Armv8aError::InvalidRegisterNumber(reg_num)).into(),
            );
        }
        self.state.register_cache[reg_num as usize] = Some((value, true));

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        if self.num_breakpoints.is_none() {
            let address = Eddfr::get_mmio_address(self.base_address);
            let eddfr = Eddfr(self.memory.read_word_32(address)?);

            self.num_breakpoints = Some(eddfr.brps() + 1);
        }
        Ok(self.num_breakpoints.unwrap())
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        // Breakpoints are always on with v7-A
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u64) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;
        let bp_control_addr =
            Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;
        let mut bp_control = Dbgbcr(0);

        // Breakpoint type - address match
        bp_control.set_bt(0b0000);
        // Match on all modes
        bp_control.set_hmc(true);
        bp_control.set_pmc(0b11);
        // Match on all bytes
        bp_control.set_bas(0b1111);
        // Enable
        bp_control.set_e(true);

        let addr_low = addr as u32;
        let addr_high = (addr >> 32) as u32;

        self.memory.write_word_32(bp_value_addr, addr_low)?;
        self.memory.write_word_32(bp_value_addr + 4, addr_high)?;
        self.memory
            .write_word_32(bp_control_addr, bp_control.into())?;

        Ok(())
    }

    fn registers(&self) -> &'static RegisterFile {
        // TODO 64-bit - this will need to be conditional based on the current CPU mode
        &ARM_REGISTER_FILE
    }

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;
        let bp_control_addr =
            Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;

        // TODO 64-bit - update value to a 64-bit write
        self.memory.write_word_32(bp_value_addr, 0)?;
        self.memory.write_word_32(bp_control_addr, 0)?;

        Ok(())
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        true
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn core_type(&self) -> CoreType {
        CoreType::Armv8a
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        if self.state.is_64_bit {
            return Err(Error::Other(anyhow!("64-bit not currently supported")));
        }

        let cpsr: u32 = self.read_core_reg(CoreRegisterAddress(16))?.try_into()?;

        // CPSR bit 5 - T - Thumb mode
        match (cpsr >> 5) & 1 {
            1 => Ok(InstructionSet::Thumb2),
            _ => Ok(InstructionSet::A32),
        }
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, Error> {
        // determine current state
        let address = Edscr::get_mmio_address(self.base_address);
        let edscr = Edscr(self.memory.read_word_32(address)?);

        if edscr.halted() {
            let reason = edscr.halt_reason();

            self.state.current_state = CoreStatus::Halted(reason);
            self.state.is_64_bit = edscr.currently_64_bit();

            return Ok(CoreStatus::Halted(reason));
        }
        // Core is neither halted nor sleeping, so we assume it is running.
        if self.state.current_state.is_halted() {
            log::warn!("Core is running, but we expected it to be halted");
        }

        self.state.current_state = CoreStatus::Running;

        Ok(CoreStatus::Running)
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;

        // TODO 64-bit - this is actually a 64-bit value in all cases, regardless of CPU mode
        // When 64-bit is supported this needs updated to read the upper bits
        for bp_unit_index in 0..num_hw_breakpoints {
            let bp_value_addr =
                Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;
            let mut bp_value = self.memory.read_word_32(bp_value_addr)? as u64;
            bp_value |= (self.memory.read_word_32(bp_value_addr + 4)? as u64) << 32;

            let bp_control_addr =
                Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * 16) as u64;
            let bp_control = Dbgbcr(self.memory.read_word_32(bp_control_addr)?);

            if bp_control.e() {
                breakpoints.push(Some(bp_value));
            } else {
                breakpoints.push(None);
            }
        }
        Ok(breakpoints)
    }
}

impl<'probe> MemoryInterface for Armv8a<'probe> {
    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        let address = valid_32_address(address)?;

        if self.state.is_64_bit {
            return Err(Error::Other(anyhow!("64-bit not currently supported")));
        }

        // Save r0, r1
        self.prepare_for_clobber(0)?;
        self.prepare_for_clobber(1)?;

        // Load r0 with the address to read from
        self.set_reg_value(0, address)?;

        // Read data to r1 - LDR r1, [r0], #4
        let instruction = build_ldr(1, 0, 4);

        self.execute_instruction(instruction)?;

        // Move from r1 to transfer buffer - MCR p14, 0, r1, c0, c5, 0
        let instruction = build_mcr(14, 0, 1, 0, 5, 0);
        self.execute_instruction_with_result(instruction)
    }
    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        // Find the word this is in and its byte offset
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        // Read the word
        let data = self.read_word_32(word_start)?;

        // Return the byte
        Ok(data.to_le_bytes()[byte_offset as usize])
    }
    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        for (i, word) in data.iter_mut().enumerate() {
            *word = self.read_word_32(address + ((i as u64) * 4))?;
        }

        Ok(())
    }
    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = self.read_word_8(address + (i as u64))?;
        }

        Ok(())
    }
    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        let address = valid_32_address(address)?;

        if self.state.is_64_bit {
            return Err(Error::Other(anyhow!("64-bit not currently supported")));
        }

        // Save r0, r1
        self.prepare_for_clobber(0)?;
        self.prepare_for_clobber(1)?;

        // Load r0 with the address to write to
        self.set_reg_value(0, address)?;
        self.set_reg_value(1, data)?;

        // Write data to memory - STR r1, [r0], #4
        let instruction = build_str(1, 0, 4);

        self.execute_instruction(instruction)?;

        Ok(())
    }
    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        // Find the word this is in and its byte offset
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        // Get the current word value
        let current_word = self.read_word_32(word_start)?;
        let mut word_bytes = current_word.to_le_bytes();
        word_bytes[byte_offset as usize] = data;

        self.write_word_32(word_start, u32::from_le_bytes(word_bytes))
    }
    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        for (i, word) in data.iter().enumerate() {
            self.write_word_32(address + ((i as u64) * 4), *word)?;
        }

        Ok(())
    }
    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        for (i, byte) in data.iter().enumerate() {
            self.write_word_8(address + ((i as u64) * 4), *byte)?;
        }

        Ok(())
    }
    fn flush(&mut self) -> Result<(), Error> {
        // Nothing to do - this runs through the CPU which automatically handles any caching
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::architecture::arm::{
        ap::MemoryAp, communication_interface::SwdSequence,
        memory::adi_v5_memory_interface::ArmProbe, sequences::DefaultArmSequence, ApAddress,
        DpAddress,
    };

    use super::*;

    const TEST_BASE_ADDRESS: u64 = 0x8000_1000;
    const TEST_CTI_ADDRESS: u64 = 0x8000_2000;

    fn address_to_reg_num(address: u64) -> u32 {
        ((address - TEST_BASE_ADDRESS) / 4) as u32
    }

    pub struct ExpectedMemoryOp {
        read: bool,
        address: u64,
        value: u32,
    }

    pub struct MockProbe {
        expected_ops: Vec<ExpectedMemoryOp>,
    }

    impl MockProbe {
        pub fn new() -> Self {
            MockProbe {
                expected_ops: vec![],
            }
        }

        pub fn expected_read(&mut self, addr: u64, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: true,
                address: addr,
                value: value,
            });
        }

        pub fn expected_write(&mut self, addr: u64, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: false,
                address: addr,
                value: value,
            });
        }
    }

    impl ArmProbe for MockProbe {
        fn read_8(&mut self, _ap: MemoryAp, _address: u64, _data: &mut [u8]) -> Result<(), Error> {
            todo!()
        }

        fn read_32(&mut self, _ap: MemoryAp, address: u64, data: &mut [u32]) -> Result<(), Error> {
            if self.expected_ops.len() == 0 {
                panic!(
                    "Received unexpected read_32 op: register {:#}",
                    address_to_reg_num(address)
                );
            }

            assert_eq!(data.len(), 1);

            let expected_op = self.expected_ops.remove(0);

            assert_eq!(
                expected_op.read,
                true,
                "R/W mismatch for register: Expected {:#} Actual: {:#}",
                address_to_reg_num(expected_op.address),
                address_to_reg_num(address)
            );
            assert_eq!(
                expected_op.address,
                address,
                "Read from unexpected register: Expected {:#} Actual: {:#}",
                address_to_reg_num(expected_op.address),
                address_to_reg_num(address)
            );

            data[0] = expected_op.value;

            Ok(())
        }

        fn write_8(&mut self, _ap: MemoryAp, _address: u64, _data: &[u8]) -> Result<(), Error> {
            todo!()
        }

        fn write_32(&mut self, _ap: MemoryAp, address: u64, data: &[u32]) -> Result<(), Error> {
            if self.expected_ops.len() == 0 {
                panic!(
                    "Received unexpected write_32 op: register {:#}",
                    address_to_reg_num(address)
                );
            }

            assert_eq!(data.len(), 1);

            let expected_op = self.expected_ops.remove(0);

            assert_eq!(expected_op.read, false);
            assert_eq!(
                expected_op.address,
                address,
                "Write to unexpected register: Expected {:#} Actual: {:#}",
                address_to_reg_num(expected_op.address),
                address_to_reg_num(address)
            );

            assert_eq!(
                expected_op.value, data[0],
                "Write value mismatch Expected {:#X} Actual: {:#X}",
                expected_op.value, data[0]
            );

            Ok(())
        }

        fn flush(&mut self) -> Result<(), Error> {
            todo!()
        }

        fn get_arm_communication_interface(
            &mut self,
        ) -> Result<
            &mut crate::architecture::arm::ArmCommunicationInterface<
                crate::architecture::arm::communication_interface::Initialized,
            >,
            Error,
        > {
            todo!()
        }
    }

    impl SwdSequence for MockProbe {
        fn swj_sequence(&mut self, _bit_len: u8, _bits: u64) -> Result<(), Error> {
            todo!()
        }

        fn swj_pins(
            &mut self,
            _pin_out: u32,
            _pin_select: u32,
            _pin_wait: u32,
        ) -> Result<u32, Error> {
            todo!()
        }
    }

    fn add_status_expectations(probe: &mut MockProbe, halted: bool) {
        let mut edscr = Edscr(0);
        edscr.set_status(if halted { 0b010011 } else { 0b000010 });
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
    }

    fn add_read_reg_expectations(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_mcr(14, 0, reg, 0, 5, 0)),
        );
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
        probe.expected_read(Dbgdtrtx::get_mmio_address(TEST_BASE_ADDRESS), value);
    }

    fn add_read_pc_expectations(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_mrc(15, 3, 0, 4, 5, 1)),
        );
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
        add_read_reg_expectations(probe, 0, value);
    }

    fn add_read_cpsr_expectations(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_mrc(15, 3, 0, 4, 5, 0)),
        );
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
        add_read_reg_expectations(probe, 0, value);
    }

    fn add_halt_expectations(probe: &mut MockProbe) {
        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(0, 1);

        probe.expected_write(CtiGate::get_mmio_address(TEST_CTI_ADDRESS), cti_gate.into());

        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(0, 1);

        probe.expected_write(
            CtiApppulse::get_mmio_address(TEST_CTI_ADDRESS),
            pulse.into(),
        );
    }

    fn add_halt_cleanup_expectations(probe: &mut MockProbe) {
        let cti_gate = CtiGate(0);

        probe.expected_write(CtiGate::get_mmio_address(TEST_CTI_ADDRESS), cti_gate.into());
    }

    fn add_resume_expectations(probe: &mut MockProbe) {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);

        probe.expected_write(CtiIntack::get_mmio_address(TEST_CTI_ADDRESS), ack.into());

        let status = CtiTrigoutstatus(0);
        probe.expected_read(
            CtiTrigoutstatus::get_mmio_address(TEST_CTI_ADDRESS),
            status.into(),
        );

        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(1, 1);
        probe.expected_write(CtiGate::get_mmio_address(TEST_CTI_ADDRESS), cti_gate.into());

        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(1, 1);
        probe.expected_write(
            CtiApppulse::get_mmio_address(TEST_CTI_ADDRESS),
            pulse.into(),
        );

        let mut edprsr = Edprsr(0);
        edprsr.set_sdr(true);
        probe.expected_read(Edprsr::get_mmio_address(TEST_BASE_ADDRESS), edprsr.into());
    }

    fn add_resume_cleanup_expectations(probe: &mut MockProbe) {
        let cti_gate = CtiGate(0);
        probe.expected_write(CtiGate::get_mmio_address(TEST_CTI_ADDRESS), cti_gate.into());
    }

    fn add_idr_expectations(probe: &mut MockProbe, bp_count: u32) {
        let mut eddfr = Eddfr(0);
        eddfr.set_brps(bp_count - 1);
        probe.expected_read(Eddfr::get_mmio_address(TEST_BASE_ADDRESS), eddfr.into());
    }

    fn add_set_r0_expectation(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_rxfull(true);

        probe.expected_write(Dbgdtrrx::get_mmio_address(TEST_BASE_ADDRESS), value);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_mrc(14, 0, 0, 0, 5, 0)),
        );
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
    }

    fn add_read_memory_expectations(probe: &mut MockProbe, address: u64, value: u32) {
        add_set_r0_expectation(probe, address as u32);

        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_ldr(1, 0, 4)),
        );
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        probe.expected_write(
            Editr::get_mmio_address(TEST_BASE_ADDRESS),
            prep_instr_for_itr_32(build_mcr(14, 0, 1, 0, 5, 0)),
        );
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());
        probe.expected_read(Dbgdtrtx::get_mmio_address(TEST_BASE_ADDRESS), value);
    }

    #[test]
    fn armv8a_new() {
        let mut probe = MockProbe::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let _ = Armv8a::new(
            mock_mem,
            &mut CortexAState::new(),
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();
    }

    #[test]
    fn armv8a_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        edscr.set_status(0b010011);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read false, second read true
        assert_eq!(false, armv8a.core_halted().unwrap());
        assert_eq!(true, armv8a.core_halted().unwrap());
    }

    #[test]
    fn armv8a_wait_for_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        edscr.set_status(0b010011);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Should halt on second read
        armv8a
            .wait_for_core_halted(Duration::from_millis(100))
            .unwrap();
    }

    #[test]
    fn armv8a_status_running() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(CoreStatus::Running, armv8a.status().unwrap());
    }

    #[test]
    fn armv8a_status_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b010011);
        probe.expected_read(Edscr::get_mmio_address(TEST_BASE_ADDRESS), edscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(
            CoreStatus::Halted(crate::HaltReason::Request),
            armv8a.status().unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_common() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read register
        add_read_reg_expectations(&mut probe, 2, REG_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(2)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(2)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_pc() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read PC
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_pc_expectations(&mut probe, REG_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(15)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(15)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_cpsr() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read CPSR
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_cpsr_expectations(&mut probe, REG_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(16)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(CoreRegisterAddress(16)).unwrap()
        );
    }

    #[test]
    fn armv8a_halt() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Write halt request
        add_halt_expectations(&mut probe);

        // Wait for halted
        add_status_expectations(&mut probe, true);

        // Read status
        add_status_expectations(&mut probe, true);
        add_halt_cleanup_expectations(&mut probe);

        // Read PC
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_pc_expectations(&mut probe, REG_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Verify PC
        assert_eq!(
            REG_VALUE as u64,
            armv8a.halt(Duration::from_millis(100)).unwrap().pc
        );
    }

    #[test]
    fn armv8a_run() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Write resume request
        add_resume_expectations(&mut probe);

        // Read status
        add_status_expectations(&mut probe, false);

        add_resume_cleanup_expectations(&mut probe);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv8a.run().unwrap();
    }

    #[test]
    fn armv8a_available_breakpoint_units() {
        const BP_COUNT: u32 = 4;
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(BP_COUNT, armv8a.available_breakpoint_units().unwrap());
    }

    #[test]
    fn armv8a_hw_breakpoints() {
        const BP_COUNT: u32 = 4;
        const BP1: u64 = 0x2345;
        const BP2: u64 = 0x8000_0000;
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        // Read BP values and controls
        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS), BP1 as u32);
        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + 4, 0);
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS), 1);

        probe.expected_read(
            Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (1 * 16),
            BP2 as u32,
        );
        probe.expected_read(
            Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + 4 + (1 * 16),
            0,
        );
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (1 * 16), 1);

        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (2 * 16), 0);
        probe.expected_read(
            Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + 4 + (2 * 16),
            0,
        );
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (2 * 16), 0);

        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (3 * 16), 0);
        probe.expected_read(
            Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + 4 + (3 * 16),
            0,
        );
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (3 * 16), 0);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        let results = armv8a.hw_breakpoints().unwrap();
        assert_eq!(Some(BP1), results[0]);
        assert_eq!(Some(BP2), results[1]);
        assert_eq!(None, results[2]);
        assert_eq!(None, results[3]);
    }

    #[test]
    fn armv8a_set_hw_breakpoint() {
        const BP_VALUE: u64 = 0x2345;
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Update BP value and control
        let mut dbgbcr = Dbgbcr(0);
        // Match on all modes
        dbgbcr.set_hmc(true);
        dbgbcr.set_pmc(0b11);
        // Match on all bytes
        dbgbcr.set_bas(0b1111);
        // Enable
        dbgbcr.set_e(true);

        probe.expected_write(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS), BP_VALUE as u32);
        probe.expected_write(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + 4, 0);
        probe.expected_write(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS), dbgbcr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv8a.set_hw_breakpoint(0, BP_VALUE).unwrap();
    }

    #[test]
    fn armv8a_clear_hw_breakpoint() {
        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Update BP value and control
        probe.expected_write(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS), 0);
        probe.expected_write(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS), 0);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv8a.clear_hw_breakpoint(0).unwrap();
    }

    #[test]
    fn armv8a_read_word_32() {
        const MEMORY_VALUE: u32 = 0xBA5EBA11;
        const MEMORY_ADDRESS: u64 = 0x12345678;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_reg_expectations(&mut probe, 1, 0);

        add_read_memory_expectations(&mut probe, MEMORY_ADDRESS, MEMORY_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(MEMORY_VALUE, armv8a.read_word_32(MEMORY_ADDRESS).unwrap());
    }

    #[test]
    fn armv8a_read_word_8() {
        const MEMORY_VALUE: u32 = 0xBA5EBA11;
        const MEMORY_ADDRESS: u64 = 0x12345679;
        const MEMORY_WORD_ADDRESS: u64 = 0x12345678;

        let mut probe = MockProbe::new();
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_reg_expectations(&mut probe, 1, 0);
        add_read_memory_expectations(&mut probe, MEMORY_WORD_ADDRESS, MEMORY_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(0xBA, armv8a.read_word_8(MEMORY_ADDRESS).unwrap());
    }
}
