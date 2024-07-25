//! Register types and the core interface for armv8-a

use super::{
    instructions::{
        aarch64,
        thumb2::{build_ldr, build_mcr, build_mrc, build_str, build_vmov, build_vmrs},
    },
    registers::{aarch32::AARCH32_WITH_FP_32_CORE_REGSISTERS, aarch64::AARCH64_CORE_REGSISTERS},
    CortexAState,
};
use crate::{
    architecture::arm::{
        core::armv8a_debug_regs::*, memory::adi_v5_memory_interface::ArmMemoryInterface,
        sequences::ArmDebugSequence, ArmError,
    },
    core::{
        memory_mapped_registers::MemoryMappedRegister, CoreRegisters, RegisterId, RegisterValue,
    },
    error::Error,
    memory::valid_32bit_address,
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType,
    InstructionSet, MemoryInterface,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Errors for the ARMv8-A state machine
#[derive(thiserror::Error, Debug)]
pub enum Armv8aError {
    /// Invalid register number
    #[error("Register number {0} is not valid for ARMv8-A in {1}-bit mode")]
    InvalidRegisterNumber(u16, u16),

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
    memory: Box<dyn ArmMemoryInterface + 'probe>,

    state: &'probe mut CortexAState,

    base_address: u64,

    cti_address: u64,

    sequence: Arc<dyn ArmDebugSequence>,

    num_breakpoints: Option<u32>,
}

impl<'probe> Armv8a<'probe> {
    pub(crate) fn new(
        mut memory: Box<dyn ArmMemoryInterface + 'probe>,
        state: &'probe mut CortexAState,
        base_address: u64,
        cti_address: u64,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let address = Edscr::get_mmio_address_from_base(base_address)?;
            let edscr = Edscr(memory.read_word_32(address)?);

            tracing::debug!("State when connecting: {:x?}", edscr);

            let core_state = if edscr.halted() {
                let reason = edscr.halt_reason();

                tracing::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            state.current_state = core_state;
            state.is_64_bit = edscr.currently_64_bit();
            // Always 32 FP regs for v8-a
            state.fp_reg_count = 32;
        }

        let mut core = Self {
            memory,
            state,
            base_address,
            cti_address,
            sequence,
            num_breakpoints: None,
        };

        if !core.state.initialized() {
            core.reset_register_cache();
            core.state.initialize();
        }

        Ok(core)
    }

    /// Execute an instruction
    fn execute_instruction(&mut self, instruction: u32) -> Result<Edscr, Error> {
        if !self.state.current_state.is_halted() {
            return Err(Error::Arm(Armv8aError::NotHalted.into()));
        }

        let mut final_instruction = instruction;

        if !self.state.is_64_bit {
            // ITR 32-bit instruction encoding requires swapping the half words
            final_instruction = prep_instr_for_itr_32(instruction)
        }

        // Run instruction
        let address = Editr::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(address, final_instruction)?;

        // Wait for completion
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        while !edscr.ite() {
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Check if we had any aborts, if so clear them and fail
        if edscr.err() || edscr.a() {
            let address = Edrcr::get_mmio_address_from_base(self.base_address)?;
            let mut edrcr = Edrcr(0);
            edrcr.set_cse(true);

            self.memory.write_word_32(address, edrcr.into())?;

            return Err(Error::Arm(Armv8aError::DataAbort.into()));
        }

        Ok(edscr)
    }

    /// Execute an instruction on the CPU and return the result
    fn execute_instruction_with_result_32(&mut self, instruction: u32) -> Result<u32, Error> {
        // Run instruction
        let mut edscr = self.execute_instruction(instruction)?;

        // Wait for TXfull
        while !edscr.txfull() {
            let address = Edscr::get_mmio_address_from_base(self.base_address)?;
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Read result
        let address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        let result = self.memory.read_word_32(address)?;

        Ok(result)
    }

    /// Execute an instruction on the CPU and return the result
    fn execute_instruction_with_result_64(&mut self, instruction: u32) -> Result<u64, Error> {
        // Run instruction
        let mut edscr = self.execute_instruction(instruction)?;

        // Wait for TXfull
        while !edscr.txfull() {
            let address = Edscr::get_mmio_address_from_base(self.base_address)?;
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Read result
        let address = Dbgdtrrx::get_mmio_address_from_base(self.base_address)?;
        let mut result: u64 = (self.memory.read_word_32(address)? as u64) << 32;

        let address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        result |= self.memory.read_word_32(address)? as u64;

        Ok(result)
    }

    fn execute_instruction_with_input_32(
        &mut self,
        instruction: u32,
        value: u32,
    ) -> Result<(), Error> {
        // Move value
        let address = Dbgdtrrx::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(address, value)?;

        // Wait for RXfull
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        while !edscr.rxfull() {
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn execute_instruction_with_input_64(
        &mut self,
        instruction: u32,
        value: u64,
    ) -> Result<(), Error> {
        // Move value
        let high_word = (value >> 32) as u32;
        let low_word = (value & 0xFFFF_FFFF) as u32;

        let address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(address, high_word)?;

        let address = Dbgdtrrx::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(address, low_word)?;

        // Wait for RXfull
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        while !edscr.rxfull() {
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn reset_register_cache(&mut self) {
        if self.state.is_64_bit {
            // 31 general purpose regs, SP, PC, PSR, 31 FP registers, FPSR, FPCR
            // Numbers match what GDB defines for aarch64
            self.state.register_cache = vec![None; 68];
        } else {
            // 16 general purpose regs, CPSR, 32 FP registers, FPSR
            self.state.register_cache = vec![None; 50];
        }
    }

    fn writeback_registers_aarch32(&mut self) -> Result<(), Error> {
        // Update SP, PC, CPSR first since they clobber the GP registeres
        let writeback_iter = (15u16..=16).chain(17u16..=48).chain(0u16..=14);

        for i in writeback_iter {
            if let Some((val, writeback)) = self.state.register_cache[i as usize] {
                if writeback {
                    match i {
                        0..=14 => {
                            let instruction = build_mrc(14, 0, i, 0, 5, 0);

                            self.execute_instruction_with_input_32(instruction, val.try_into()?)?;
                        }
                        15 => {
                            // Move val to r0
                            let instruction = build_mrc(14, 0, 0, 0, 5, 0);

                            self.execute_instruction_with_input_32(instruction, val.try_into()?)?;

                            // Write to DLR
                            let instruction = build_mrc(15, 3, 0, 4, 5, 1);
                            self.execute_instruction(instruction)?;
                        }
                        17..=48 => {
                            // Move value to r0, r1
                            let value: u64 = val.try_into()?;
                            let low_word = value as u32;
                            let high_word = (value >> 32) as u32;

                            let instruction = build_mrc(14, 0, 0, 0, 5, 0);
                            self.execute_instruction_with_input_32(instruction, low_word)?;

                            let instruction = build_mrc(14, 0, 1, 0, 5, 0);
                            self.execute_instruction_with_input_32(instruction, high_word)?;

                            // VMOV
                            let instruction = build_vmov(0, 0, 1, i - 17);
                            self.execute_instruction(instruction)?;
                        }
                        _ => {
                            panic!("Logic missing for writeback of register {i}");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    fn writeback_registers_aarch64(&mut self) -> Result<(), Error> {
        // Update SP, PC, CPSR, FP first since they clobber the GP registeres
        let writeback_iter = (31u16..=33).chain(34u16..=65).chain(0u16..=30);

        for i in writeback_iter {
            if let Some((val, writeback)) = self.state.register_cache[i as usize] {
                if writeback {
                    match i {
                        0..=30 => {
                            self.set_reg_value(i, val.try_into()?)?;
                        }
                        31 => {
                            // Move val to r0
                            self.set_reg_value(0, val.try_into()?)?;

                            // MSR SP_EL0, X0
                            let instruction = aarch64::build_msr(3, 0, 4, 1, 0, 0);
                            self.execute_instruction(instruction)?;
                        }
                        32 => {
                            // Move val to r0
                            self.set_reg_value(0, val.try_into()?)?;

                            // MSR DLR_EL0, X0
                            let instruction = aarch64::build_msr(3, 3, 4, 5, 1, 0);
                            self.execute_instruction(instruction)?;
                        }
                        34..=65 => {
                            let val: u128 = val.try_into()?;

                            // Move lower word to r0
                            self.set_reg_value(0, val as u64)?;

                            // INS v<x>.d[0], x0
                            let instruction = aarch64::build_ins_gp_to_fp(i - 34, 0, 0);
                            self.execute_instruction(instruction)?;

                            // Move upper word to r0
                            self.set_reg_value(0, (val >> 64) as u64)?;

                            // INS v<x>.d[0], x0
                            let instruction = aarch64::build_ins_gp_to_fp(i - 34, 0, 1);
                            self.execute_instruction(instruction)?;
                        }
                        _ => {
                            panic!("Logic missing for writeback of register {i}");
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Sync any updated registers back to the core
    fn writeback_registers(&mut self) -> Result<(), Error> {
        if self.state.is_64_bit {
            self.writeback_registers_aarch64()?;
        } else {
            self.writeback_registers_aarch32()?;
        }

        self.reset_register_cache();

        Ok(())
    }

    /// Save register if needed before it gets clobbered by instruction execution
    fn prepare_for_clobber(&mut self, reg: u16) -> Result<(), Error> {
        if self.state.register_cache[reg as usize].is_none() {
            // cache reg since we're going to clobber it
            let val = self.read_core_reg(RegisterId(reg))?;

            // Mark reg as needing writeback
            self.state.register_cache[reg as usize] = Some((val, true));
        }

        Ok(())
    }

    fn set_reg_value(&mut self, reg: u16, value: u64) -> Result<(), Error> {
        if self.state.is_64_bit {
            // MRS DBGDTR_EL0, X<n>
            let instruction = aarch64::build_mrs(2, 3, 0, 4, 0, reg);

            self.execute_instruction_with_input_64(instruction, value)
        } else {
            let value = valid_32bit_address(value)?;

            let instruction = build_mrc(14, 0, reg, 0, 5, 0);

            self.execute_instruction_with_input_32(instruction, value)
        }
    }

    fn ack_cti_halt(&mut self) -> Result<(), Error> {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);

        let address = CtiIntack::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, ack.into())?;

        loop {
            let address = CtiTrigoutstatus::get_mmio_address_from_base(self.cti_address)?;
            let trig_status = CtiTrigoutstatus(self.memory.read_word_32(address)?);

            if trig_status.status(0) == 0 {
                break;
            }
        }

        Ok(())
    }

    fn read_core_reg_32(&mut self, reg_num: u16) -> Result<RegisterValue, Error> {
        // Generate instruction to extract register
        match reg_num {
            0..=14 => {
                // r0-r14, valid
                // MCR p14, 0, <Rd>, c0, c5, 0 ; Write DBGDTRTXint Register
                let instruction = build_mcr(14, 0, reg_num, 0, 5, 0);

                let reg_value = self.execute_instruction_with_result_32(instruction)?;

                Ok(reg_value.into())
            }
            15 => {
                // PC, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC p15, 3, r0, c4, c5, 1 ; Read DLR to r0
                let instruction = build_mrc(15, 3, 0, 4, 5, 1);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let pc = self.execute_instruction_with_result_32(instruction)?;

                Ok(pc.into())
            }
            16 => {
                // CPSR, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC c15, 3, r0, c4, c5, 0
                let instruction = build_mrc(15, 3, 0, 4, 5, 0);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let cpsr = self.execute_instruction_with_result_32(instruction)?;

                Ok(cpsr.into())
            }
            17..=48 => {
                // Access via r0, r1
                self.prepare_for_clobber(0)?;
                self.prepare_for_clobber(1)?;

                // VMOV r0, r1, <reg>
                let instruction = build_vmov(1, 0, 1, reg_num - 17);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let mut value = self.execute_instruction_with_result_32(instruction)? as u64;

                // Read from r1
                let instruction = build_mcr(14, 0, 1, 0, 5, 0);
                value |= (self.execute_instruction_with_result_32(instruction)? as u64) << 32;

                Ok(value.into())
            }
            49 => {
                // Access via r0
                self.prepare_for_clobber(0)?;

                // VMRS r0, FPSCR
                let instruction = build_vmrs(0, 1);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let value = self.execute_instruction_with_result_32(instruction)?;

                Ok(value.into())
            }
            _ => Err(Error::Arm(
                Armv8aError::InvalidRegisterNumber(reg_num, 32).into(),
            )),
        }
    }

    fn read_core_reg_64(&mut self, reg_num: u16) -> Result<RegisterValue, Error> {
        match reg_num {
            0..=30 => {
                // GP register

                // MSR DBGDTR_EL0, X<n>
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, reg_num);

                let reg_value = self.execute_instruction_with_result_64(instruction)?;

                Ok(reg_value.into())
            }
            31 => {
                // SP
                self.prepare_for_clobber(0)?;

                // MRS SP_EL0, X0
                let instruction = aarch64::build_mrs(3, 0, 4, 1, 0, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let pc = self.execute_instruction_with_result_64(instruction)?;

                Ok(pc.into())
            }
            32 => {
                // PC, must access via x0
                self.prepare_for_clobber(0)?;

                // MRS DLR_EL0, X0
                let instruction = aarch64::build_mrs(3, 3, 4, 5, 1, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let sp = self.execute_instruction_with_result_64(instruction)?;

                Ok(sp.into())
            }
            33 => {
                // PSR
                self.prepare_for_clobber(0)?;

                // MRS DSPSR_EL0, X0
                let instruction = aarch64::build_mrs(3, 3, 4, 5, 0, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let psr: u32 = self.execute_instruction_with_result_64(instruction)? as u32;

                Ok(psr.into())
            }
            34..=65 => {
                // v0-v31
                self.prepare_for_clobber(0)?;

                // MOV x0, v<x>.d[0]
                let instruction = aarch64::build_ins_fp_to_gp(0, reg_num - 34, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let mut value: u128 = self.execute_instruction_with_result_64(instruction)? as u128;

                // MOV x0, v<x>.d[1]
                let instruction = aarch64::build_ins_fp_to_gp(0, reg_num - 34, 1);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                value |= (self.execute_instruction_with_result_64(instruction)? as u128) << 64;

                Ok(value.into())
            }
            66 => {
                // FPSR
                self.prepare_for_clobber(0)?;

                // MRS FPSR, X0
                let instruction = aarch64::build_mrs(3, 3, 4, 4, 1, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let fpsr: u32 = self.execute_instruction_with_result_64(instruction)? as u32;

                Ok(fpsr.into())
            }
            67 => {
                // FPCR
                self.prepare_for_clobber(0)?;

                // MRS FPCR, X0
                let instruction = aarch64::build_mrs(3, 3, 4, 4, 0, 0);
                self.execute_instruction(instruction)?;

                // Read from x0
                let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
                let fpsr: u32 = self.execute_instruction_with_result_64(instruction)? as u32;

                Ok(fpsr.into())
            }
            _ => Err(Error::Arm(
                Armv8aError::InvalidRegisterNumber(reg_num, 64).into(),
            )),
        }
    }

    fn with_core_halted<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Self) -> Result<R, Error>,
    {
        // save halt status
        let original_halt_status = self.state.current_state.is_halted();
        if !original_halt_status {
            self.halt(Duration::from_millis(100))?;
        }

        let result = f(self);

        // restore halt status
        if !original_halt_status {
            self.run()?;
        }
        result
    }

    fn with_memory_access_mode<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Self) -> Result<R, Error>,
    {
        // enable memory access(MA) mode
        self.set_memory_access_mode(true)?;

        let result = f(self);

        // disable memory access(MA) mode
        self.set_memory_access_mode(false)?;

        result
    }

    fn read_cpu_memory_aarch32_32(&mut self, address: u64) -> Result<u32, Error> {
        let address = valid_32bit_address(address)?;

        self.with_core_halted(|armv8a| {
            // Save r0, r1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load r0 with the address to read from
            armv8a.set_reg_value(0, address.into())?;

            // Read data to r1 - LDR r1, [r0], #4
            let instruction = build_ldr(1, 0, 4);

            armv8a.execute_instruction(instruction)?;

            // Move from r1 to transfer buffer - MCR p14, 0, r1, c0, c5, 0
            let instruction = build_mcr(14, 0, 1, 0, 5, 0);
            armv8a.execute_instruction_with_result_32(instruction)
        })
    }

    fn read_cpu_memory_aarch64_bytes(
        &mut self,
        address: u64,
        data: &mut [u8],
    ) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            // Save x0, x1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load x0 with the address to read from
            armv8a.set_reg_value(0, address)?;

            for d in data {
                // Read data to w1 - LDRB w1, [x0], #1
                let instruction = aarch64::build_ldrb(1, 0, 1);

                armv8a.execute_instruction(instruction)?;

                // MSR DBGDTRTX_EL0, X1
                let instruction = aarch64::build_msr(2, 3, 0, 5, 0, 1);
                *d = armv8a.execute_instruction_with_result_32(instruction)? as u8;
            }

            Ok(())
        })
    }

    fn read_cpu_memory_aarch64_32(&mut self, address: u64) -> Result<u32, Error> {
        self.with_core_halted(|armv8a| {
            // Save x0, x1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load x0 with the address to read from
            armv8a.set_reg_value(0, address)?;

            // Read data to w1 - LDR w1, [x0], #4
            let instruction = aarch64::build_ldrw(1, 0, 4);

            armv8a.execute_instruction(instruction)?;

            // MSR DBGDTRTX_EL0, X1
            let instruction = aarch64::build_msr(2, 3, 0, 5, 0, 1);
            armv8a.execute_instruction_with_result_32(instruction)
        })
    }

    fn read_cpu_memory_aarch64_64(&mut self, address: u64) -> Result<u64, Error> {
        self.with_core_halted(|armv8a| {
            // Save x0, x1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load x0 with the address to read from
            armv8a.set_reg_value(0, address)?;

            // Read data to x1 - LDR x1, [x0], #8
            let instruction = aarch64::build_ldr(1, 0, 8);

            armv8a.execute_instruction(instruction)?;

            // MSR DBGDTR_EL0, X1
            let instruction = aarch64::build_msr(2, 3, 0, 4, 0, 1);
            armv8a.execute_instruction_with_result_64(instruction)
        })
    }

    fn write_cpu_memory_aarch32_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        let address = valid_32bit_address(address)?;
        self.with_core_halted(|armv8a| {
            // Save r0, r1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load x0 with the address to write to
            armv8a.set_reg_value(0, address.into())?;
            armv8a.set_reg_value(1, data.into())?;

            // Write data to memory - STR r1, [r0], #4
            let instruction = build_str(1, 0, 4);

            armv8a.execute_instruction(instruction)?;
            Ok(())
        })
    }

    fn write_cpu_memory_aarch64_bytes(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            // Save r0, r1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load x0 with the address to write to
            armv8a.set_reg_value(0, address)?;

            for d in data {
                armv8a.set_reg_value(1, u64::from(*d))?;

                // Write data to memory - STRB w1, [r0], #1
                let instruction = aarch64::build_strb(1, 0, 4);

                armv8a.execute_instruction(instruction)?;
            }
            Ok(())
        })
    }

    fn write_cpu_memory_aarch64_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            // Save x0, x1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load r0 with the address to write to
            armv8a.set_reg_value(0, address)?;
            armv8a.set_reg_value(1, data.into())?;

            // Write data to memory - STR x1, [x0], #4
            let instruction = aarch64::build_strw(1, 0, 4);

            armv8a.execute_instruction(instruction)?;
            Ok(())
        })
    }

    fn write_cpu_memory_aarch64_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            // Save x0, x1
            armv8a.prepare_for_clobber(0)?;
            armv8a.prepare_for_clobber(1)?;

            // Load r0 with the address to write to
            armv8a.set_reg_value(0, address)?;
            armv8a.set_reg_value(1, data)?;

            // Write data to memory - STR x1, [x0], #8
            let instruction = aarch64::build_str(1, 0, 8);

            armv8a.execute_instruction(instruction)?;
            Ok(())
        })
    }

    fn set_memory_access_mode(&mut self, enable_ma_mode: bool) -> Result<(), Error> {
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr: Edscr = Edscr(self.memory.read_word_32(address)?);
        edscr.set_ma(enable_ma_mode);
        self.memory.write_word_32(address, edscr.into())?;

        Ok(())
    }

    fn write_cpu_memory_aarch64_fast(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            let (prefix, aligned, suffix) = armv8a.aligned_to_32(address, data);
            let mut address = address;

            // write unaligned part
            if !prefix.is_empty() {
                armv8a.write_cpu_memory_aarch64_bytes(address, prefix)?;
                address += u64::try_from(prefix.len()).unwrap();
            }

            // write aligned part
            armv8a.write_cpu_memory_aarch64_fast_inner(address, aligned)?;
            address += u64::try_from(aligned.len()).unwrap();

            // write unaligned part
            if !suffix.is_empty() {
                armv8a.write_cpu_memory_aarch64_bytes(address, suffix)?;
            }
            Ok(())
        })
    }

    /// Fast data download method
    /// ref. ARM DDI 0487D.a, K9-7312, Figure K9-1 Fast data download in AArch64 state
    fn write_cpu_memory_aarch64_fast_inner(
        &mut self,
        address: u64,
        data: &[u8],
    ) -> Result<(), Error> {
        // assume only call from write_cpu_memory_aarch64_fast
        if data.is_empty() {
            return Ok(());
        }
        if data.len() % 4 != 0 || address % 4 != 0 {
            return Err(Error::MemoryNotAligned {
                address,
                alignment: 4,
            });
        }

        // Save x0
        self.prepare_for_clobber(0)?;

        // Load r0 with the address to write to
        self.set_reg_value(0, address)?;

        self.with_memory_access_mode(|armv8a| {
            for d in data.chunks(4) {
                let word = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
                // memory write loop
                let dbgdtr_rx_address = Dbgdtrrx::get_mmio_address_from_base(armv8a.base_address)?;
                armv8a.memory.write_word_32(dbgdtr_rx_address, word)?;
            }
            Ok(())
        })?;

        // error check
        let edscr_address = Edscr::get_mmio_address_from_base(self.base_address)?;
        if Edscr(self.memory.read_word_32(edscr_address)?).err() {
            // under-run or abort

            // clear error flag
            let edrcr_address = Edrcr::get_mmio_address_from_base(self.base_address)?;
            let mut edrcr = Edrcr(0);
            edrcr.set_cse(true);
            self.memory.write_word_32(edrcr_address, edrcr.into())?;

            return Err(Error::Arm(ArmError::Armv8a(Armv8aError::DataAbort)));
        }

        Ok(())
    }

    fn aligned_to_32_split_offset(&self, address: u64, data: &[u8]) -> (usize, usize) {
        // rounding up
        let word_aligned_address = (address + 3) & (!0x03u64);
        let unaligned_prefix_size = usize::try_from(word_aligned_address - address).unwrap();
        let unaligned_suffix_size =
            usize::try_from((address + u64::try_from(data.len()).unwrap()) % 4).unwrap();
        let word_aligned_size = data.len() - (unaligned_prefix_size + unaligned_suffix_size);

        (unaligned_prefix_size, word_aligned_size)
    }

    fn aligned_to_32_mut<'a>(
        &self,
        address: u64,
        data: &'a mut [u8],
    ) -> (&'a mut [u8], &'a mut [u8], &'a mut [u8]) {
        // take out 32-bit aligned part
        let (unaligned_prefix_size, word_aligned_size) =
            self.aligned_to_32_split_offset(address, data);

        // take out 32-bit aligned part
        let (prefix, rest) = data.split_at_mut(unaligned_prefix_size);
        let (aligned, suffix) = rest.split_at_mut(word_aligned_size);
        (prefix, aligned, suffix)
    }

    fn aligned_to_32<'a>(&self, address: u64, data: &'a [u8]) -> (&'a [u8], &'a [u8], &'a [u8]) {
        // take out 32-bit aligned part
        let (unaligned_prefix_size, word_aligned_size) =
            self.aligned_to_32_split_offset(address, data);

        // take out 32-bit aligned part
        let (prefix, rest) = data.split_at(unaligned_prefix_size);
        let (aligned, suffix) = rest.split_at(word_aligned_size);
        (prefix, aligned, suffix)
    }

    fn read_cpu_memory_aarch64_fast(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.with_core_halted(|armv8a| {
            let (prefix, aligned, suffix) = armv8a.aligned_to_32_mut(address, data);
            let mut address = address;

            // read unaligned part
            if !prefix.is_empty() {
                armv8a.read_cpu_memory_aarch64_bytes(address, prefix)?;
                address += u64::try_from(prefix.len()).unwrap();
            }

            // read aligned part
            armv8a.read_cpu_memory_aarch64_fast_inner(address, aligned)?;
            address += u64::try_from(aligned.len()).unwrap();

            // read unaligned part
            if !suffix.is_empty() {
                armv8a.read_cpu_memory_aarch64_bytes(address, suffix)?;
            }

            Ok(())
        })
    }

    /// Fast data download method
    /// ref. ARM DDI 0487D.a, K9-7313, Figure K9-2 Fast data upload in AArch64 state
    fn read_cpu_memory_aarch64_fast_inner(
        &mut self,
        address: u64,
        data: &mut [u8],
    ) -> Result<(), Error> {
        // assume only call from read_cpu_memory_aarch64_fast
        if data.is_empty() {
            return Ok(());
        }
        if data.len() % 4 != 0 || address % 4 != 0 {
            return Err(Error::MemoryNotAligned {
                address,
                alignment: 4,
            });
        }

        // Save x0
        self.prepare_for_clobber(0)?;

        // Load x0 with the address to read from
        self.set_reg_value(0, address)?;

        // set "MSR DBGDTR_EL0, X0" opcode to EDITR
        let msr_instruction = aarch64::build_msr(2, 3, 0, 4, 0, 0);
        let editr_address = Editr::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(editr_address, msr_instruction)?;

        // wait for TXfull == 1
        let edscr_address = Edscr::get_mmio_address_from_base(self.base_address)?;
        while !{ Edscr(self.memory.read_word_32(edscr_address)?) }.txfull() {}

        let dbgdtr_tx_address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        let (data, last) = data.split_at_mut(data.len() - std::mem::size_of::<u32>());

        self.with_memory_access_mode(|armv8a| {
            // discard firtst 32bit
            let _ = armv8a.memory.read_word_32(dbgdtr_tx_address)?;
            for d in data.chunks_mut(4) {
                // memory read loop
                let tmp = armv8a.memory.read_word_32(dbgdtr_tx_address)?.to_le_bytes();
                d.copy_from_slice(&tmp);
            }

            Ok(())
        })?;

        // read last 32bit
        let l = self.memory.read_word_32(dbgdtr_tx_address)?.to_le_bytes();
        last.copy_from_slice(&l);

        // error check
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let edscr = Edscr(self.memory.read_word_32(address)?);
        if edscr.err() {
            // clear error flag
            let edrcr_address = Edrcr::get_mmio_address_from_base(self.base_address)?;
            let mut edrcr = Edrcr(0);
            edrcr.set_cse(true);
            self.memory.write_word_32(edrcr_address, edrcr.into())?;

            Err(Error::Arm(ArmError::Armv8a(Armv8aError::DataAbort)))
        } else {
            Ok(())
        }
    }

    fn set_core_status(&mut self, new_status: CoreStatus) {
        super::update_core_status(&mut self.memory, &mut self.state.current_state, new_status);
    }
}

impl<'probe> CoreInterface for Armv8a<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while !self.core_halted()? {
            if start.elapsed() >= timeout {
                return Err(Error::Arm(ArmError::Timeout));
            }
            // Wait a bit before polling again.
            std::thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let edscr = Edscr(self.memory.read_word_32(address)?);

        Ok(edscr.halted())
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, Error> {
        // determine current state
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let edscr = Edscr(self.memory.read_word_32(address)?);

        if edscr.halted() {
            let reason = edscr.halt_reason();

            self.set_core_status(CoreStatus::Halted(reason));
            self.state.is_64_bit = edscr.currently_64_bit();

            return Ok(CoreStatus::Halted(reason));
        }
        // Core is neither halted nor sleeping, so we assume it is running.
        if self.state.current_state.is_halted() {
            tracing::warn!("Core is running, but we expected it to be halted");
        }

        self.set_core_status(CoreStatus::Running);

        Ok(CoreStatus::Running)
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        if !matches!(self.state.current_state, CoreStatus::Halted(_)) {
            // Ungate halt CTI channel
            let mut cti_gate = CtiGate(0);
            cti_gate.set_en(0, 1);

            let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
            self.memory.write_word_32(address, cti_gate.into())?;

            // Pulse it
            let mut pulse = CtiApppulse(0);
            pulse.set_apppulse(0, 1);

            let address = CtiApppulse::get_mmio_address_from_base(self.cti_address)?;
            self.memory.write_word_32(address, pulse.into())?;

            // Wait for halt
            self.wait_for_core_halted(timeout)?;

            // Reset our cached values
            self.reset_register_cache();
        }

        // Update core status
        let _ = self.status()?;

        // Gate halt channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, cti_gate.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn run(&mut self) -> Result<(), Error> {
        if matches!(self.state.current_state, CoreStatus::Running) {
            return Ok(());
        }

        // set writeback values
        self.writeback_registers()?;

        self.ack_cti_halt()?;

        // Ungate restart CTI channel
        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(1, 1);

        let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, cti_gate.into())?;

        // Pulse it
        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(1, 1);

        let address = CtiApppulse::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, pulse.into())?;

        // Wait for ack
        let address = Edprsr::get_mmio_address_from_base(self.base_address)?;

        loop {
            let edprsr = Edprsr(self.memory.read_word_32(address)?);
            if edprsr.sdr() {
                break;
            }
        }

        // Recompute / verify current state
        self.set_core_status(CoreStatus::Running);
        let _ = self.status()?;

        // Gate restart channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, cti_gate.into())?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(
            &mut *self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        // Reset our cached values
        self.reset_register_cache();

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_catch_set(
            &mut *self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;
        self.sequence.reset_system(
            &mut *self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        // Release from reset
        self.sequence.reset_catch_clear(
            &mut *self.memory,
            crate::CoreType::Armv8a,
            Some(self.base_address),
        )?;

        self.wait_for_core_halted(timeout)?;

        // Update core status
        let _ = self.status()?;

        // Reset our cached values
        self.reset_register_cache();

        // try to read the program counter
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // Load EDECR, set SS bit for step mode
        let edecr_address = Edecr::get_mmio_address_from_base(self.base_address)?;
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
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let reg_num = address.0;

        // check cache
        if (reg_num as usize) < self.state.register_cache.len() {
            if let Some(cached_result) = self.state.register_cache[reg_num as usize] {
                return Ok(cached_result.0);
            }
        }

        let result = if self.state.is_64_bit {
            self.read_core_reg_64(reg_num)
        } else {
            self.read_core_reg_32(reg_num)
        };

        if let Ok(value) = result {
            self.state.register_cache[reg_num as usize] = Some((value, false));

            Ok(value)
        } else {
            Err(result.err().unwrap())
        }
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        let reg_num = address.0;
        let current_mode = if self.state.is_64_bit { 64 } else { 32 };

        if (reg_num as usize) >= self.state.register_cache.len() {
            return Err(Error::Arm(
                Armv8aError::InvalidRegisterNumber(reg_num, current_mode).into(),
            ));
        }
        self.state.register_cache[reg_num as usize] = Some((value, true));

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        if self.num_breakpoints.is_none() {
            let address = Eddfr::get_mmio_address_from_base(self.base_address)?;
            let eddfr = Eddfr(self.memory.read_word_32(address)?);

            self.num_breakpoints = Some(eddfr.brps() + 1);
        }
        Ok(self.num_breakpoints.unwrap())
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;

        for bp_unit_index in 0..num_hw_breakpoints {
            let bp_value_addr = Dbgbvr::get_mmio_address_from_base(self.base_address)?
                + (bp_unit_index * 16) as u64;
            let mut bp_value = self.memory.read_word_32(bp_value_addr)? as u64;
            bp_value |= (self.memory.read_word_32(bp_value_addr + 4)? as u64) << 32;

            let bp_control_addr = Dbgbcr::get_mmio_address_from_base(self.base_address)?
                + (bp_unit_index * 16) as u64;
            let bp_control = Dbgbcr(self.memory.read_word_32(bp_control_addr)?);

            if bp_control.e() {
                breakpoints.push(Some(bp_value));
            } else {
                breakpoints.push(None);
            }
        }
        Ok(breakpoints)
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        // Breakpoints are always on with v7-A
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u64) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;
        let bp_control_addr =
            Dbgbcr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;
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

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;
        let bp_control_addr =
            Dbgbcr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;

        self.memory.write_word_32(bp_value_addr, 0)?;
        self.memory.write_word_32(bp_value_addr + 4, 0)?;
        self.memory.write_word_32(bp_control_addr, 0)?;

        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        if self.state.is_64_bit {
            &AARCH64_CORE_REGSISTERS
        } else {
            &AARCH32_WITH_FP_32_CORE_REGSISTERS
        }
    }

    fn program_counter(&self) -> &'static CoreRegister {
        if self.state.is_64_bit {
            &super::registers::aarch64::PC
        } else {
            &super::registers::cortex_m::PC
        }
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        if self.state.is_64_bit {
            &super::registers::aarch64::FP
        } else {
            &super::registers::cortex_m::FP
        }
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        if self.state.is_64_bit {
            &super::registers::aarch64::SP
        } else {
            &super::registers::cortex_m::SP
        }
    }

    fn return_address(&self) -> &'static CoreRegister {
        if self.state.is_64_bit {
            &super::registers::aarch64::RA
        } else {
            &super::registers::cortex_m::RA
        }
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
            Ok(InstructionSet::A64)
        } else {
            let cpsr: u32 = self.read_core_reg(RegisterId(16))?.try_into()?;

            // CPSR bit 5 - T - Thumb mode
            match (cpsr >> 5) & 1 {
                1 => Ok(InstructionSet::Thumb2),
                _ => Ok(InstructionSet::A32),
            }
        }
    }

    fn fpu_support(&mut self) -> Result<bool, crate::error::Error> {
        // Always available for v8-a
        Ok(true)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, crate::error::Error> {
        // Always available for v8-a
        Ok(self.state.fp_reg_count)
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_set(
            &mut *self.memory,
            CoreType::Armv8a,
            Some(self.base_address),
        )?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_clear(
            &mut *self.memory,
            CoreType::Armv8a,
            Some(self.base_address),
        )?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn debug_core_stop(&mut self) -> Result<(), Error> {
        if matches!(self.state.current_state, CoreStatus::Halted(_)) {
            // We may have clobbered registers we wrote during debugging
            // Best effort attempt to put them back before we exit
            self.writeback_registers()?;
        }

        self.sequence
            .debug_core_stop(&mut *self.memory, CoreType::Armv8a)?;

        Ok(())
    }

    fn is_64_bit(&self) -> bool {
        self.state.is_64_bit
    }
}

impl<'probe> MemoryInterface for Armv8a<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.state.is_64_bit
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        if self.state.is_64_bit {
            self.read_cpu_memory_aarch64_64(address)
        } else {
            let mut ret = self.read_cpu_memory_aarch32_32(address)? as u64;
            ret |= (self.read_cpu_memory_aarch32_32(address + 4)? as u64) << 32;

            Ok(ret)
        }
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        if self.state.is_64_bit {
            self.read_cpu_memory_aarch64_32(address)
        } else {
            self.read_cpu_memory_aarch32_32(address)
        }
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        // Find the word this is in and its byte offset
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        // Read the word
        let data = self.read_word_32(word_start)?;

        // Return the byte
        Ok((data >> (byte_offset * 8)) as u16)
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

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };
            self.read_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter_mut().enumerate() {
                *word = self.read_word_64(address + ((i as u64) * 8))?;
            }
        }

        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };
            self.read_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter_mut().enumerate() {
                *word = self.read_word_32(address + ((i as u64) * 4))?;
            }
        }

        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };
            self.read_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter_mut().enumerate() {
                *word = self.read_word_16(address + ((i as u64) * 2))?;
            }
        }

        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        if self.state.is_64_bit {
            self.read_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, byte) in data.iter_mut().enumerate() {
                *byte = self.read_word_8(address + (i as u64))?;
            }
        }

        Ok(())
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        if self.state.is_64_bit {
            self.write_cpu_memory_aarch64_64(address, data)
        } else {
            let low_word = data as u32;
            let high_word = (data >> 32) as u32;

            self.write_cpu_memory_aarch32_32(address, low_word)?;
            self.write_cpu_memory_aarch32_32(address + 4, high_word)
        }
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        if self.state.is_64_bit {
            self.write_cpu_memory_aarch64_32(address, data)
        } else {
            self.write_cpu_memory_aarch32_32(address, data)
        }
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        // Find the word this is in and its byte offset
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        // Get the current word value
        let mut word = self.read_word_32(word_start)?;

        // patch the word into it
        word &= !(0xFFFFu32 << (byte_offset * 8));
        word |= (data as u32) << (byte_offset * 8);

        self.write_word_32(word_start, word)
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

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
            self.write_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter().enumerate() {
                self.write_word_64(address + ((i as u64) * 8), *word)?;
            }
        }

        Ok(())
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
            self.write_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter().enumerate() {
                self.write_word_32(address + ((i as u64) * 4), *word)?;
            }
        }

        Ok(())
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        if self.state.is_64_bit {
            let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
            self.write_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, word) in data.iter().enumerate() {
                self.write_word_16(address + ((i as u64) * 2), *word)?;
            }
        }

        Ok(())
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if self.state.is_64_bit {
            self.write_cpu_memory_aarch64_fast(address, data)?;
        } else {
            for (i, byte) in data.iter().enumerate() {
                tracing::info!("writing {:?} bytes", i);
                self.write_word_8(address + (i as u64), *byte)?;
            }
        }

        Ok(())
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(false)
    }

    fn flush(&mut self) -> Result<(), Error> {
        // Nothing to do - this runs through the CPU which automatically handles any caching
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::{
        architecture::arm::{
            ap::MemoryAp, communication_interface::SwdSequence, sequences::DefaultArmSequence,
        },
        probe::DebugProbeError,
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
        is_64_bit: bool,
    }

    impl MockProbe {
        pub fn new(is_64_bit: bool) -> Self {
            MockProbe {
                expected_ops: vec![],
                is_64_bit,
            }
        }

        pub fn expected_read(&mut self, addr: u64, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: true,
                address: addr,
                value,
            });
        }

        pub fn expected_write(&mut self, addr: u64, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: false,
                address: addr,
                value,
            });
        }
    }

    impl ArmMemoryInterface for MockProbe {
        fn update_core_status(&mut self, _: CoreStatus) {}

        fn read_8(&mut self, _address: u64, _data: &mut [u8]) -> Result<(), ArmError> {
            todo!()
        }

        fn read_16(&mut self, _address: u64, _data: &mut [u16]) -> Result<(), ArmError> {
            todo!()
        }

        fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), ArmError> {
            if self.expected_ops.is_empty() {
                panic!(
                    "Received unexpected read_32 op: register {:#}",
                    address_to_reg_num(address)
                );
            }

            assert_eq!(data.len(), 1);

            let expected_op = self.expected_ops.remove(0);

            assert!(
                expected_op.read,
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

        fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), ArmError> {
            todo!()
        }

        fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), ArmError> {
            todo!()
        }

        fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), ArmError> {
            if self.expected_ops.is_empty() {
                panic!(
                    "Received unexpected write_32 op: register {:#}",
                    address_to_reg_num(address)
                );
            }

            assert_eq!(data.len(), 1);

            let expected_op = self.expected_ops.remove(0);

            assert!(!expected_op.read);
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

        fn flush(&mut self) -> Result<(), ArmError> {
            todo!()
        }

        fn ap(&mut self) -> MemoryAp {
            todo!()
        }

        fn get_arm_communication_interface(
            &mut self,
        ) -> Result<
            &mut crate::architecture::arm::ArmCommunicationInterface<
                crate::architecture::arm::communication_interface::Initialized,
            >,
            DebugProbeError,
        > {
            Err(DebugProbeError::NotImplemented {
                function_name: "get_arm_communication_interface",
            })
        }

        fn read_64(&mut self, _address: u64, _data: &mut [u64]) -> Result<(), ArmError> {
            todo!()
        }

        fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), ArmError> {
            todo!()
        }

        fn supports_8bit_transfers(&self) -> Result<bool, ArmError> {
            Ok(false)
        }

        fn supports_native_64bit_access(&mut self) -> bool {
            false
        }
    }

    impl SwdSequence for MockProbe {
        fn swj_sequence(&mut self, _bit_len: u8, _bits: u64) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn swj_pins(
            &mut self,
            _pin_out: u32,
            _pin_select: u32,
            _pin_wait: u32,
        ) -> Result<u32, DebugProbeError> {
            todo!()
        }
    }

    fn add_status_expectations(probe: &mut MockProbe, halted: bool) {
        let mut edscr = Edscr(0);
        edscr.set_status(if halted { 0b010011 } else { 0b000010 });
        if probe.is_64_bit {
            edscr.set_rw(0b1111);
        }
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
    }

    fn add_read_reg_expectations(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_mcr(14, 0, reg, 0, 5, 0)),
        );
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
    }

    fn add_read_reg_64_expectations(probe: &mut MockProbe, reg: u16, value: u64) {
        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_msr(2, 3, 0, 4, 0, reg),
        );
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        probe.expected_read(
            Dbgdtrrx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            (value >> 32) as u32,
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value as u32,
        );
    }

    fn add_read_pc_expectations(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_mrc(15, 3, 0, 4, 5, 1)),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        add_read_reg_expectations(probe, 0, value);
    }

    fn add_read_pc_64_expectations(probe: &mut MockProbe, value: u64) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_mrs(3, 3, 4, 5, 1, 0),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        add_read_reg_64_expectations(probe, 0, value);
    }

    fn add_read_cpsr_expectations(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_mrc(15, 3, 0, 4, 5, 0)),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        add_read_reg_expectations(probe, 0, value);
    }

    fn add_read_cpsr_64_expectations(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_mrs(3, 3, 4, 5, 0, 0),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        add_read_reg_64_expectations(probe, 0, value.into());
    }

    fn add_halt_expectations(probe: &mut MockProbe) {
        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(0, 1);

        probe.expected_write(
            CtiGate::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            cti_gate.into(),
        );

        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(0, 1);

        probe.expected_write(
            CtiApppulse::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            pulse.into(),
        );
    }

    fn add_halt_cleanup_expectations(probe: &mut MockProbe) {
        let cti_gate = CtiGate(0);

        probe.expected_write(
            CtiGate::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            cti_gate.into(),
        );
    }

    fn add_resume_expectations(probe: &mut MockProbe) {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);

        probe.expected_write(
            CtiIntack::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            ack.into(),
        );

        let status = CtiTrigoutstatus(0);
        probe.expected_read(
            CtiTrigoutstatus::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            status.into(),
        );

        let mut cti_gate = CtiGate(0);
        cti_gate.set_en(1, 1);
        probe.expected_write(
            CtiGate::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            cti_gate.into(),
        );

        let mut pulse = CtiApppulse(0);
        pulse.set_apppulse(1, 1);
        probe.expected_write(
            CtiApppulse::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            pulse.into(),
        );

        let mut edprsr = Edprsr(0);
        edprsr.set_sdr(true);
        probe.expected_read(
            Edprsr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edprsr.into(),
        );
    }

    fn add_resume_cleanup_expectations(probe: &mut MockProbe) {
        let cti_gate = CtiGate(0);
        probe.expected_write(
            CtiGate::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            cti_gate.into(),
        );
    }

    fn add_idr_expectations(probe: &mut MockProbe, bp_count: u32) {
        let mut eddfr = Eddfr(0);
        eddfr.set_brps(bp_count - 1);
        probe.expected_read(
            Eddfr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            eddfr.into(),
        );
    }

    fn add_set_r0_expectation(probe: &mut MockProbe, value: u32) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_rxfull(true);

        probe.expected_write(
            Dbgdtrrx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_mrc(14, 0, 0, 0, 5, 0)),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
    }

    fn add_set_x0_expectation(probe: &mut MockProbe, value: u64) {
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_rxfull(true);

        probe.expected_write(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            (value >> 32) as u32,
        );
        probe.expected_write(
            Dbgdtrrx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value as u32,
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_mrs(2, 3, 0, 4, 0, 0),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
    }

    fn add_read_memory_expectations(probe: &mut MockProbe, address: u64, value: u32) {
        add_set_r0_expectation(probe, address as u32);

        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_ldr(1, 0, 4)),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            prep_instr_for_itr_32(build_mcr(14, 0, 1, 0, 5, 0)),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
    }

    fn add_read_memory_aarch64_expectations(probe: &mut MockProbe, address: u64, value: u32) {
        add_set_x0_expectation(probe, address);

        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_ldrw(1, 0, 4),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        probe.expected_write(
            Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            aarch64::build_msr(2, 3, 0, 5, 0, 1),
        );
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
    }

    #[test]
    fn armv8a_new() {
        let mut probe = MockProbe::new(false);

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mock_mem = Box::new(probe) as _;

        let mut state = CortexAState::new();

        let core = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert!(!core.state.is_64_bit);
    }

    #[test]
    fn armv8a_core_halted() {
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        edscr.set_status(0b010011);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv8a = Armv8a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read false, second read true
        assert!(!armv8a.core_halted().unwrap());
        assert!(armv8a.core_halted().unwrap());
    }

    #[test]
    fn armv8a_wait_for_core_halted() {
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        edscr.set_status(0b010011);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b000010);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut edscr = Edscr(0);
        edscr.set_status(0b010011);
        probe.expected_read(
            Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            edscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

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

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read register
        add_read_reg_expectations(&mut probe, 2, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(2)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(2)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_common_64() {
        const REG_VALUE: u64 = 0xFFFF_EEEE_0000_ABCD;

        let mut probe = MockProbe::new(true);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read register
        add_read_reg_64_expectations(&mut probe, 2, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(2)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(2)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_pc() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read PC
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_pc_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(15)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(15)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_64_reg_pc() {
        const REG_VALUE: u64 = 0xFFFF_EEEE_0000_ABCD;

        let mut probe = MockProbe::new(true);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read PC
        add_read_reg_64_expectations(&mut probe, 0, 0);
        add_read_pc_64_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(32)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(32)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_reg_cpsr() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read CPSR
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_cpsr_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(16)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(16)).unwrap()
        );
    }

    #[test]
    fn armv8a_read_core_64_reg_cpsr() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new(true);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read CPSR
        add_read_reg_64_expectations(&mut probe, 0, 0);
        add_read_cpsr_64_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

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
            armv8a.read_core_reg(RegisterId(33)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv8a.read_core_reg(RegisterId(33)).unwrap()
        );
    }

    #[test]
    fn armv8a_halt() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, false);

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

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Write resume request
        add_resume_expectations(&mut probe);

        // Read status
        add_status_expectations(&mut probe, false);

        add_resume_cleanup_expectations(&mut probe);

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        // Read BP values and controls
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            BP1 as u32,
        );
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4,
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            1,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 16,
            BP2 as u32,
        );
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4 + 16,
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 16,
            1,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (2 * 16),
            0,
        );
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4 + (2 * 16),
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (2 * 16),
            0,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (3 * 16),
            0,
        );
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4 + (3 * 16),
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (3 * 16),
            0,
        );

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
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

        probe.expected_write(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            BP_VALUE as u32,
        );
        probe.expected_write(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4,
            0,
        );
        probe.expected_write(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgbcr.into(),
        );

        let mock_mem = Box::new(probe) as _;

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
        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Update BP value and control
        probe.expected_write(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            0,
        );
        probe.expected_write(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4,
            0,
        );
        probe.expected_write(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            0,
        );

        let mock_mem = Box::new(probe) as _;

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

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_reg_expectations(&mut probe, 1, 0);

        add_read_memory_expectations(&mut probe, MEMORY_ADDRESS, MEMORY_VALUE);

        let mock_mem = Box::new(probe) as _;

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
    fn armv8a_read_word_32_aarch64() {
        const MEMORY_VALUE: u32 = 0xBA5EBA11;
        const MEMORY_ADDRESS: u64 = 0x12345678;

        let mut probe = MockProbe::new(true);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_64_expectations(&mut probe, 0, 0);
        add_read_reg_64_expectations(&mut probe, 1, 0);

        add_read_memory_aarch64_expectations(&mut probe, MEMORY_ADDRESS, MEMORY_VALUE);

        let mock_mem = Box::new(probe) as _;

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

        let mut probe = MockProbe::new(false);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_reg_expectations(&mut probe, 1, 0);
        add_read_memory_expectations(&mut probe, MEMORY_WORD_ADDRESS, MEMORY_VALUE);

        let mock_mem = Box::new(probe) as _;

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

    #[test]
    fn armv8a_read_word_aarch64_8() {
        const MEMORY_VALUE: u32 = 0xBA5EBA11;
        const MEMORY_ADDRESS: u64 = 0x12345679;
        const MEMORY_WORD_ADDRESS: u64 = 0x12345678;

        let mut probe = MockProbe::new(true);
        let mut state = CortexAState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read memory
        add_read_reg_64_expectations(&mut probe, 0, 0);
        add_read_reg_64_expectations(&mut probe, 1, 0);
        add_read_memory_aarch64_expectations(&mut probe, MEMORY_WORD_ADDRESS, MEMORY_VALUE);

        let mock_mem = Box::new(probe) as _;

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
