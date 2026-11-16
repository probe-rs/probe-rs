//! Register types and the core interface for armv8-r

use super::{
    CortexARState,
    armv8a::Armv8aError,
    instructions::thumb2::{build_ldr, build_mcr, build_mrc, build_str, build_vmov, build_vmrs},
    registers::aarch32::AARCH32_WITH_FP_32_CORE_REGISTERS,
};
use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType,
    InstructionSet, MemoryInterface,
    architecture::arm::{
        ArmError, core::armv8a_debug_regs::*, memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
    core::{
        CoreRegisters, RegisterId, RegisterValue, memory_mapped_registers::MemoryMappedRegister,
    },
    error::Error,
    memory::{MemoryNotAlignedError, valid_32bit_address},
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const OPERATION_TIMEOUT: Duration = Duration::from_millis(250);

/// Register cache size: r0-r15, CPSR, D0-D31, FPSCR.
const REGISTER_CACHE_SIZE: usize = 50;
const REG_PC: u16 = 15;
const REG_CPSR: u16 = 16;
const REG_D0: u16 = 17;
const REG_D31: u16 = 48;
const REG_FPSCR: u16 = 49;

/// EDITR takes a 32-bit T32 instruction with its two halfwords swapped.
fn prep_instr_for_itr_32(instruction: u32) -> u32 {
    ((instruction & 0xFFFF) << 16) | ((instruction & 0xFFFF_0000) >> 16)
}

/// Interface for interacting with an Armv8-R core.
pub struct Armv8r<'probe> {
    memory: Box<dyn ArmMemoryInterface + 'probe>,
    state: &'probe mut CortexARState,
    base_address: u64,
    cti_address: u64,
    sequence: Arc<dyn ArmDebugSequence>,
    num_breakpoints: Option<u32>,
}

impl<'probe> Armv8r<'probe> {
    pub(crate) fn new(
        mut memory: Box<dyn ArmMemoryInterface + 'probe>,
        state: &'probe mut CortexARState,
        base_address: u64,
        cti_address: u64,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        let mut connect_edscr = None;

        if !state.initialized() {
            // determine current state
            let address = Edscr::get_mmio_address_from_base(base_address)?;
            let edscr = Edscr(memory.read_word_32(address)?);
            connect_edscr = Some(edscr);

            tracing::debug!("State when connecting: {:x?}", edscr);

            let core_state = if edscr.halted() {
                let reason = edscr.halt_reason();

                tracing::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            state.current_state = core_state;
            state.is_64_bit = false;
            // D0-D31
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

            if let Some(edscr) = connect_edscr
                && edscr.halted()
                && (edscr.rxfull() || edscr.txfull() || edscr.err())
                && let Err(error) = core.drain_stale_dcc(edscr)
            {
                tracing::warn!("Could not clear stale debug communication state: {error}");
            }
        }

        Ok(core)
    }

    /// Clear debug communication state left behind by an interrupted debugger session.
    fn drain_stale_dcc(&mut self, edscr: Edscr) -> Result<(), Error> {
        tracing::warn!(
            "Core halted with stale debug communication state ({edscr:x?}), clearing it"
        );

        if edscr.err() {
            self.clear_sticky_error()?;
        }

        // An external read of DBGDTRTX empties it.
        if edscr.txfull() {
            let dbgdtrtx_address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
            let _ = self.memory.read_word_32(dbgdtrtx_address)?;
        }

        // Empty DBGDTRRX: consume it into r0
        // MRC p14, 0, R0, c0, c5, 0
        if edscr.rxfull() {
            self.prepare_for_clobber(0)?;
            self.execute_instruction(build_mrc(14, 0, 0, 0, 5, 0))?;
        }

        Ok(())
    }

    /// Clrar EDSCR.ERR.
    fn clear_sticky_error(&mut self) -> Result<(), Error> {
        let address = Edrcr::get_mmio_address_from_base(self.base_address)?;
        let mut edrcr = Edrcr(0);

        edrcr.set_cse(true);
        self.memory.write_word_32(address, edrcr.into())?;

        Ok(())
    }

    /// Execute an instruction.
    fn execute_instruction(&mut self, instruction: u32) -> Result<Edscr, Error> {
        if !self.state.current_state.is_halted() {
            return Err(Error::Arm(Armv8aError::NotHalted.into()));
        }

        // Run instruction
        let address = Editr::get_mmio_address_from_base(self.base_address)?;
        self.memory
            .write_word_32(address, prep_instr_for_itr_32(instruction))?;

        // Wait for completion
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr = Edscr(self.memory.read_word_32(address)?);

        let start = Instant::now();
        while !edscr.ite() {
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Arm(ArmError::Timeout));
            }
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Check if we had any aborts, if so clear them and fail
        if edscr.err() || edscr.a() {
            self.clear_sticky_error()?;

            return Err(Error::Arm(Armv8aError::DataAbort.into()));
        }

        Ok(edscr)
    }

    /// Execute an instruction on the CPU and return the result.
    fn execute_instruction_with_result(&mut self, instruction: u32) -> Result<u32, Error> {
        // Run instruction
        let mut edscr = self.execute_instruction(instruction)?;

        // Wait for TXfull
        let start = Instant::now();

        while !edscr.txfull() {
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
            let address = Edscr::get_mmio_address_from_base(self.base_address)?;
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Read result
        let address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        let result = self.memory.read_word_32(address)?;

        Ok(result)
    }

    fn execute_instruction_with_input(
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

        let start = Instant::now();

        while !edscr.rxfull() {
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
            edscr = Edscr(self.memory.read_word_32(address)?);
        }

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn reset_register_cache(&mut self) {
        self.state.register_cache = vec![None; REGISTER_CACHE_SIZE];
    }

    /// Sync any updated registers back to the core.
    fn writeback_registers(&mut self) -> Result<(), Error> {
        // PC, CPSR and the FP registers first, since they clobber the GP registers.
        let writeback_iter = (REG_PC..=REG_CPSR).chain(REG_D0..=REG_D31).chain(0u16..=14);

        for i in writeback_iter {
            if let Some((val, writeback)) = self.state.register_cache[i as usize]
                && writeback
            {
                match i {
                    0..=14 => {
                        // MRC p14, 0, R<i>, c0, c5, 0
                        let instruction = build_mrc(14, 0, i, 0, 5, 0);
                        self.execute_instruction_with_input(instruction, val.try_into()?)?;
                    }
                    REG_PC => {
                        self.set_reg_value(0, val.try_into()?)?;

                        // DLR, Debug Link Register: in Debug state, the address to restart from.
                        // MCR p15, 3, R0, c4, c5, 1
                        self.execute_instruction(build_mcr(15, 3, 0, 4, 5, 1))?;
                    }
                    REG_CPSR => {
                        self.set_reg_value(0, val.try_into()?)?;

                        // MCR p15, 3, R0, c4, c5, 0
                        self.execute_instruction(build_mcr(15, 3, 0, 4, 5, 0))?;
                    }
                    REG_D0..=REG_D31 => {
                        let value: u64 = val.try_into()?;
                        self.set_reg_value(0, value as u32)?;
                        self.set_reg_value(1, (value >> 32) as u32)?;

                        // VMOV D<n>, R0, R1
                        self.execute_instruction(build_vmov(0, 0, 1, i - REG_D0))?;
                    }
                    _ => {
                        panic!("Logic missing for writeback of register {i}");
                    }
                }
            }
        }

        self.reset_register_cache();

        Ok(())
    }

    /// Save register if needed before it gets clobbered by instruction execution.
    fn prepare_for_clobber(&mut self, reg: u16) -> Result<(), Error> {
        if let Some(val) = &mut self.state.register_cache[reg as usize] {
            // Mark reg as needing writeback
            val.1 = true;
        } else {
            // cache reg since we're going to clobber it
            let val = self.read_core_reg(RegisterId(reg))?;

            // Mark reg as needing writeback
            self.state.register_cache[reg as usize] = Some((val, true));
        }

        Ok(())
    }

    /// Load a GP register through DBGDTRRX. MRC p14, 0, R<reg>, c0, c5, 0.
    fn set_reg_value(&mut self, reg: u16, value: u32) -> Result<(), Error> {
        self.execute_instruction_with_input(build_mrc(14, 0, reg, 0, 5, 0), value)
    }

    /// Read a GP register through DBGDTRTX. MCR p14, 0, R<reg>, c0, c5, 0.
    fn get_reg_value(&mut self, reg: u16) -> Result<u32, Error> {
        self.execute_instruction_with_result(build_mcr(14, 0, reg, 0, 5, 0))
    }

    fn ack_cti_halt(&mut self) -> Result<(), Error> {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);

        let address = CtiIntack::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, ack.into())?;

        let start = Instant::now();
        loop {
            let address = CtiTrigoutstatus::get_mmio_address_from_base(self.cti_address)?;
            let trig_status = CtiTrigoutstatus(self.memory.read_word_32(address)?);

            if trig_status.status(0) == 0 {
                break;
            }
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
        }

        Ok(())
    }

    fn read_core_reg_uncached(&mut self, reg_num: u16) -> Result<RegisterValue, Error> {
        match reg_num {
            0..=14 => Ok(self.get_reg_value(reg_num)?.into()),
            REG_PC => {
                // PC, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC p15, 3, R0, c4, c5, 1 ; read DLR
                self.execute_instruction(build_mrc(15, 3, 0, 4, 5, 1))?;

                Ok(self.get_reg_value(0)?.into())
            }
            REG_CPSR => {
                // CPSR, must access via r0
                self.prepare_for_clobber(0)?;

                // MRC p15, 3, R0, c4, c5, 0 ; read DSPSR
                self.execute_instruction(build_mrc(15, 3, 0, 4, 5, 0))?;

                Ok(self.get_reg_value(0)?.into())
            }
            REG_D0..=REG_D31 => {
                // Access via r0, r1
                self.prepare_for_clobber(0)?;
                self.prepare_for_clobber(1)?;

                // VMOV R0, R1, D<n>
                self.execute_instruction(build_vmov(1, 0, 1, reg_num - REG_D0))?;

                let mut value = self.get_reg_value(0)? as u64;
                value |= (self.get_reg_value(1)? as u64) << 32;

                Ok(value.into())
            }
            REG_FPSCR => {
                // Access via r0
                self.prepare_for_clobber(0)?;

                // VMRS R0, FPSCR
                self.execute_instruction(build_vmrs(0, 1))?;

                Ok(self.get_reg_value(0)?.into())
            }
            _ => Err(Error::Arm(
                Armv8aError::InvalidRegisterNumber(reg_num, 32).into(),
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

    /// Run `f` with EDSCR.MA set. In memory access mode a DBGDTRRX write stores a word through R0
    /// and a DBGDTRTX read loads the next one, both post-incrementing R0 and clobbering R1.
    fn with_memory_access_mode<F, R>(&mut self, f: F) -> Result<R, Error>
    where
        F: FnOnce(&mut Self) -> Result<R, Error>,
    {
        self.set_memory_access_mode(true)?;

        let result = f(self);

        self.set_memory_access_mode(false)?;

        result
    }

    fn set_memory_access_mode(&mut self, enable_ma_mode: bool) -> Result<(), Error> {
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let mut edscr: Edscr = Edscr(self.memory.read_word_32(address)?);

        edscr.set_ma(enable_ma_mode);
        self.memory.write_word_32(address, edscr.into())?;

        Ok(())
    }

    /// Fail with a data abort if EDSCR.ERR is set after a memory-access-mode transfer.
    fn check_transfer_error(&mut self) -> Result<(), Error> {
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        if Edscr(self.memory.read_word_32(address)?).err() {
            // under-run, over-run or abort
            self.clear_sticky_error()?;

            return Err(Error::Arm(ArmError::Armv8a(Armv8aError::DataAbort)));
        }

        Ok(())
    }

    fn read_cpu_memory_32(&mut self, address: u64) -> Result<u32, Error> {
        let address = valid_32bit_address(address)?;

        self.with_core_halted(|core| {
            core.prepare_for_clobber(0)?;
            core.prepare_for_clobber(1)?;

            core.set_reg_value(0, address)?;

            // LDR R1, [R0], #4
            core.execute_instruction(build_ldr(1, 0, 4))?;

            core.get_reg_value(1)
        })
    }

    fn write_cpu_memory_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        let address = valid_32bit_address(address)?;

        self.with_core_halted(|core| {
            core.prepare_for_clobber(0)?;
            core.prepare_for_clobber(1)?;
            core.set_reg_value(0, address)?;
            core.set_reg_value(1, data)?;
            // STR R1, [R0], #4
            core.execute_instruction(build_str(1, 0, 4))?;

            Ok(())
        })
    }

    /// Splits an access of `len` bytes at `address` into the size of the unaligned prefix and
    /// the size of the 32-bit aligned main section. The remaining bytes are the unaligned suffix.
    ///
    /// An access that does not reach the next word boundary is entirely prefix.
    fn aligned_to_32_split_offset(address: u64, len: usize) -> (usize, usize) {
        // bytes up to the next word boundary without exceeding 32-bits.
        let unaligned_prefix_size = usize::from(address.wrapping_neg() as u8 % 4).min(len);
        let word_aligned_size = (len - unaligned_prefix_size) & !0x03;

        (unaligned_prefix_size, word_aligned_size)
    }

    fn aligned_to_32_mut<'a>(
        &self,
        address: u64,
        data: &'a mut [u8],
    ) -> (&'a mut [u8], &'a mut [u8], &'a mut [u8]) {
        let (unaligned_prefix_size, word_aligned_size) =
            Self::aligned_to_32_split_offset(address, data.len());
        let (prefix, rest) = data.split_at_mut(unaligned_prefix_size);
        let (aligned, suffix) = rest.split_at_mut(word_aligned_size);

        (prefix, aligned, suffix)
    }

    fn aligned_to_32<'a>(&self, address: u64, data: &'a [u8]) -> (&'a [u8], &'a [u8], &'a [u8]) {
        let (unaligned_prefix_size, word_aligned_size) =
            Self::aligned_to_32_split_offset(address, data.len());
        let (prefix, rest) = data.split_at(unaligned_prefix_size);
        let (aligned, suffix) = rest.split_at(word_aligned_size);

        (prefix, aligned, suffix)
    }

    fn write_cpu_memory_fast(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.with_core_halted(|core| {
            let (prefix, aligned, suffix) = core.aligned_to_32(address, data);
            let mut address = address;

            for d in prefix {
                core.write_word_8(address, *d)?;
                address += 1;
            }

            core.write_cpu_memory_fast_inner(address, aligned)?;
            address += u64::try_from(aligned.len()).unwrap();

            for d in suffix {
                core.write_word_8(address, *d)?;
                address += 1;
            }

            Ok(())
        })
    }

    /// Fast data download: R0 holds the address, each DBGDTRRX write stores one word.
    /// ref. ARM DDI 0487D.a, K9-7312, Figure K9-1 (AArch64; AArch32 uses R0/R1).
    fn write_cpu_memory_fast_inner(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let address = valid_32bit_address(address)?;
        if !data.len().is_multiple_of(4) || !address.is_multiple_of(4) {
            return Err(MemoryNotAlignedError {
                address: address.into(),
                alignment: 4,
            }
            .into());
        }

        self.prepare_for_clobber(0)?;
        self.prepare_for_clobber(1)?;

        self.set_reg_value(0, address)?;

        let dbgdtrrx_address = Dbgdtrrx::get_mmio_address_from_base(self.base_address)?;

        self.with_memory_access_mode(|core| {
            for d in data.chunks(4) {
                // "MRC p14, 0, R1, c0, c5, 0" then "STR R1, [R0], #4"
                let word = u32::from_le_bytes([d[0], d[1], d[2], d[3]]);
                core.memory.write_word_32(dbgdtrrx_address, word)?;
            }

            Ok(())
        })?;

        self.check_transfer_error()
    }

    /// Read memory, using memory access mode for the word-aligned part.
    fn read_cpu_memory_fast(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.with_core_halted(|core| {
            let (prefix, aligned, suffix) = core.aligned_to_32_mut(address, data);
            let mut address = address;

            for byte in prefix.iter_mut() {
                *byte = core.read_word_8(address)?;
                address += 1;
            }

            core.read_cpu_memory_fast_inner(address, aligned)?;
            address += u64::try_from(aligned.len()).unwrap();

            for byte in suffix.iter_mut() {
                *byte = core.read_word_8(address)?;
                address += 1;
            }

            Ok(())
        })
    }

    /// Fast data upload: R0 holds the address, each DBGDTRRX read returns one word.
    /// ref. ARM DDI 0487D.a, K9-7313, Figure K9-2 (AArch64; AArch32 uses R0/R1).
    fn read_cpu_memory_fast_inner(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        if data.is_empty() {
            return Ok(());
        }
        let address = valid_32bit_address(address)?;
        if !data.len().is_multiple_of(4) || !address.is_multiple_of(4) {
            return Err(MemoryNotAlignedError {
                address: address.into(),
                alignment: 4,
            }
            .into());
        }

        self.prepare_for_clobber(0)?;
        self.prepare_for_clobber(1)?;

        self.set_reg_value(0, address)?;

        // Prime DBGDTRTX with R0 so TXfull is set; in memory access mode each DBGDTRTX read then
        // returns its value and loads the next word ("LDR R1, [R0], #4"). The priming instruction
        // is written to EDITR directly, so it needs the same halfword swap as any other.
        // MCR p14, 0, R0, c0, c5, 0
        let editr_address = Editr::get_mmio_address_from_base(self.base_address)?;
        self.memory.write_word_32(
            editr_address,
            prep_instr_for_itr_32(build_mcr(14, 0, 0, 0, 5, 0)),
        )?;

        // wait for TXfull
        let edscr_address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let start = Instant::now();
        while !Edscr(self.memory.read_word_32(edscr_address)?).txfull() {
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
        }

        let dbgdtrtx_address = Dbgdtrtx::get_mmio_address_from_base(self.base_address)?;
        let (data, last) = data.split_at_mut(data.len() - std::mem::size_of::<u32>());

        self.with_memory_access_mode(|core| {
            // The first read returns the primed address; discard it.
            let _ = core.memory.read_word_32(dbgdtrtx_address)?;
            for d in data.chunks_mut(4) {
                let word = core.memory.read_word_32(dbgdtrtx_address)?.to_le_bytes();
                d.copy_from_slice(&word);
            }

            Ok(())
        })?;

        // The last word, read outside memory access mode so no further load is triggered.
        let word = self.memory.read_word_32(dbgdtrtx_address)?.to_le_bytes();
        last.copy_from_slice(&word);

        self.check_transfer_error()
    }

    fn set_core_status(&mut self, new_status: CoreStatus) {
        super::update_core_status(&mut self.memory, &mut self.state.current_state, new_status);
    }
}

impl CoreInterface for Armv8r<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        let start = Instant::now();

        while !self.core_halted()? {
            if start.elapsed() >= timeout {
                return Err(Error::Arm(ArmError::Timeout));
            }
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
        let address = Edscr::get_mmio_address_from_base(self.base_address)?;
        let edscr = Edscr(self.memory.read_word_32(address)?);

        if edscr.halted() {
            let reason = edscr.halt_reason();

            self.set_core_status(CoreStatus::Halted(reason));

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

            self.wait_for_core_halted(timeout)?;

            self.reset_register_cache();
        }

        let _ = self.status()?;

        // Gate halt channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, cti_gate.into())?;

        let pc_value = self.read_core_reg(self.program_counter().into())?;

        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn run(&mut self) -> Result<(), Error> {
        if matches!(self.state.current_state, CoreStatus::Running) {
            return Ok(());
        }

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

        let start = Instant::now();
        loop {
            let edprsr = Edprsr(self.memory.read_word_32(address)?);
            if edprsr.sdr() {
                break;
            }
            if start.elapsed() > OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
        }

        self.set_core_status(CoreStatus::Running);
        let _ = self.status()?;

        // Gate restart channel
        let cti_gate = CtiGate(0);

        let address = CtiGate::get_mmio_address_from_base(self.cti_address)?;
        self.memory.write_word_32(address, cti_gate.into())?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence
            .reset_system(&mut *self.memory, CoreType::Armv8r, Some(self.base_address))?;

        self.reset_register_cache();

        self.set_core_status(CoreStatus::Running);
        let _ = self.status()?;

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_catch_set(
            &mut *self.memory,
            CoreType::Armv8r,
            Some(self.base_address),
        )?;
        self.sequence
            .reset_system(&mut *self.memory, CoreType::Armv8r, Some(self.base_address))?;

        // Release from reset
        self.sequence.reset_catch_clear(
            &mut *self.memory,
            CoreType::Armv8r,
            Some(self.base_address),
        )?;

        self.wait_for_core_halted(timeout)?;

        let _ = self.status()?;

        self.reset_register_cache();

        let pc_value = self.read_core_reg(self.program_counter().into())?;

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

        self.run()?;

        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Reset EDECR
        edecr.set_ss(false);
        self.memory.write_word_32(edecr_address, edecr.into())?;

        let pc_value = self.read_core_reg(self.program_counter().into())?;

        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let reg_num = address.0;

        if (reg_num as usize) < self.state.register_cache.len()
            && let Some(cached_result) = self.state.register_cache[reg_num as usize]
        {
            return Ok(cached_result.0);
        }

        let value = self.read_core_reg_uncached(reg_num)?;
        self.state.register_cache[reg_num as usize] = Some((value, false));

        Ok(value)
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        let reg_num = address.0;

        if (reg_num as usize) >= self.state.register_cache.len() {
            return Err(Error::Arm(
                Armv8aError::InvalidRegisterNumber(reg_num, 32).into(),
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
            let bp_value = self.memory.read_word_32(bp_value_addr)? as u64;

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
        // Breakpoints are always on
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u64) -> Result<(), Error> {
        let addr = valid_32bit_address(addr)?;
        let bp_value_addr =
            Dbgbvr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;
        let bp_control_addr =
            Dbgbcr::get_mmio_address_from_base(self.base_address)? + (bp_unit_index * 16) as u64;
        let mut bp_control = Dbgbcr(0);

        // Breakpoint type: address match
        bp_control.set_bt(0b0000);
        // Match in EL2 and in EL1/EL0
        bp_control.set_hmc(true);
        bp_control.set_pmc(0b11);
        // Match on all bytes
        bp_control.set_bas(0b1111);
        // Enable
        bp_control.set_e(true);

        self.memory.write_word_32(bp_value_addr, addr)?;
        self.memory.write_word_32(bp_value_addr + 4, 0)?;
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
        &AARCH32_WITH_FP_32_CORE_REGISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &super::registers::cortex_m::PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &super::registers::cortex_m::FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &super::registers::cortex_m::SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &super::registers::cortex_m::RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        true
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn core_type(&self) -> CoreType {
        CoreType::Armv8r
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        // Reading CPSR runs instructions through EDITR, which needs the core halted.
        let cpsr: u32 = self
            .with_core_halted(|core| core.read_core_reg(RegisterId(REG_CPSR)))?
            .try_into()?;

        // CPSR bit 5 - T - Thumb mode
        match (cpsr >> 5) & 1 {
            1 => Ok(InstructionSet::Thumb2),
            _ => Ok(InstructionSet::A32),
        }
    }

    fn fpu_support(&mut self) -> Result<bool, crate::error::Error> {
        Ok(true)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, crate::error::Error> {
        Ok(self.state.fp_reg_count)
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_set(
            &mut *self.memory,
            CoreType::Armv8r,
            Some(self.base_address),
        )?;

        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_clear(
            &mut *self.memory,
            CoreType::Armv8r,
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
            .debug_core_stop(&mut *self.memory, CoreType::Armv8r)?;

        Ok(())
    }

    fn is_64_bit(&self) -> bool {
        false
    }
}

impl MemoryInterface for Armv8r<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        let mut ret = self.read_cpu_memory_32(address)? as u64;
        ret |= (self.read_cpu_memory_32(address + 4)? as u64) << 32;

        Ok(ret)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.read_cpu_memory_32(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        let byte_offset = address % 4;
        let word = self.read_word_32(address - byte_offset)?;

        Ok((word >> (byte_offset * 8)) as u16)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        let byte_offset = address % 4;
        let word = self.read_word_32(address - byte_offset)?;

        Ok(word.to_le_bytes()[byte_offset as usize])
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };

        self.read_cpu_memory_fast(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };

        self.read_cpu_memory_fast(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        let (_prefix, data, _suffix) = unsafe { data.align_to_mut::<u8>() };

        self.read_cpu_memory_fast(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.read_cpu_memory_fast(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.write_cpu_memory_32(address, data as u32)?;

        self.write_cpu_memory_32(address + 4, (data >> 32) as u32)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.write_cpu_memory_32(address, data)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        let mut word = self.read_word_32(word_start)?;
        word &= !(0xFFFFu32 << (byte_offset * 8));
        word |= (data as u32) << (byte_offset * 8);

        self.write_word_32(word_start, word)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        let byte_offset = address % 4;
        let word_start = address - byte_offset;

        let mut word_bytes = self.read_word_32(word_start)?.to_le_bytes();
        word_bytes[byte_offset as usize] = data;

        self.write_word_32(word_start, u32::from_le_bytes(word_bytes))
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        // The fast path moves 32-bit words; it does not guarantee 64-bit bus accesses.
        let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
        self.write_cpu_memory_fast(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
        self.write_cpu_memory_fast(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        let (_prefix, data, _suffix) = unsafe { data.align_to::<u8>() };
        self.write_cpu_memory_fast(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.write_cpu_memory_fast(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        Ok(false)
    }

    fn flush(&mut self) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use crate::architecture::arm::{core::mock::MockProbe, sequences::DefaultArmSequence};

    use super::*;

    const TEST_BASE_ADDRESS: u64 = 0x8000_1000;
    const TEST_CTI_ADDRESS: u64 = 0x8000_2000;

    fn edscr_address() -> u64 {
        Edscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap()
    }

    fn editr_address() -> u64 {
        Editr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap()
    }

    fn dbgdtrrx_address() -> u64 {
        Dbgdtrrx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap()
    }

    fn dbgdtrtx_address() -> u64 {
        Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap()
    }

    /// EDSCR read at connect or by `status()`. Halted means halted by an external request.
    fn add_status(probe: &mut MockProbe, halted: bool) {
        let mut edscr = Edscr(0);
        edscr.set_status(if halted { 0b010011 } else { 0b000010 });
        probe.expected_read(edscr_address(), edscr.into());
    }

    /// One instruction through EDITR, completing at once.
    fn add_execute(probe: &mut MockProbe, instruction: u32) {
        probe.expected_write(editr_address(), prep_instr_for_itr_32(instruction));
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        probe.expected_read(edscr_address(), edscr.into());
    }

    /// `get_reg_value`: MCR to DBGDTRTX, then read DBGDTRTX.
    fn add_get_reg(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(
            editr_address(),
            prep_instr_for_itr_32(build_mcr(14, 0, reg, 0, 5, 0)),
        );
        let mut edscr = Edscr(0);
        edscr.set_ite(true);
        edscr.set_txfull(true);
        probe.expected_read(edscr_address(), edscr.into());
        probe.expected_read(dbgdtrtx_address(), value);
    }

    /// `set_reg_value`: write DBGDTRRX, then MRC from it.
    fn add_set_reg(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(dbgdtrrx_address(), value);
        let mut edscr = Edscr(0);
        edscr.set_rxfull(true);
        probe.expected_read(edscr_address(), edscr.into());
        add_execute(probe, build_mrc(14, 0, reg, 0, 5, 0));
    }

    fn add_resume(probe: &mut MockProbe) {
        let mut ack = CtiIntack(0);
        ack.set_ack(0, 1);
        probe.expected_write(
            CtiIntack::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            ack.into(),
        );
        probe.expected_read(
            CtiTrigoutstatus::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            CtiTrigoutstatus(0).into(),
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

        add_status(probe, false);

        probe.expected_write(
            CtiGate::get_mmio_address_from_base(TEST_CTI_ADDRESS).unwrap(),
            CtiGate(0).into(),
        );
    }

    fn new_core(probe: MockProbe, state: &mut CortexARState) -> Armv8r<'_> {
        Armv8r::new(
            Box::new(probe),
            state,
            TEST_BASE_ADDRESS,
            TEST_CTI_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap()
    }

    #[test]
    fn armv8r_new() {
        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);

        let mut state = CortexARState::new();
        let core = new_core(probe, &mut state);

        assert_eq!(core.core_type(), CoreType::Armv8r);
        assert!(!core.is_64_bit());
        assert!(core.state.current_state.is_halted());
    }

    #[test]
    fn armv8r_new_drains_stale_rx() {
        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);

        // Halted at connect, with a word left in DBGDTRRX.
        let mut edscr = Edscr(0);
        edscr.set_status(0b010011);
        edscr.set_rxfull(true);
        probe.expected_read(edscr_address(), edscr.into());

        // r0 is saved, then DBGDTRRX is consumed into it.
        add_get_reg(&mut probe, 0, 0x1234);
        add_execute(&mut probe, build_mrc(14, 0, 0, 0, 5, 0));

        let mut state = CortexARState::new();
        let core = new_core(probe, &mut state);

        // r0 is marked for writeback so resuming restores it.
        assert_eq!(
            core.state.register_cache[0],
            Some((RegisterValue::from(0x1234u32), true))
        );
    }

    #[test]
    fn armv8r_status() {
        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);
        add_status(&mut probe, false);
        add_status(&mut probe, true);

        let mut state = CortexARState::new();
        let mut core = new_core(probe, &mut state);

        assert_eq!(CoreStatus::Running, core.status().unwrap());
        assert_eq!(
            CoreStatus::Halted(crate::HaltReason::Request),
            core.status().unwrap()
        );
    }

    #[test]
    fn armv8r_read_core_reg() {
        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);
        add_get_reg(&mut probe, 3, 0xABCD);

        let mut state = CortexARState::new();
        let mut core = new_core(probe, &mut state);

        assert_eq!(
            RegisterValue::from(0xABCDu32),
            core.read_core_reg(RegisterId(3)).unwrap()
        );
        // Cached: no further accesses.
        assert_eq!(
            RegisterValue::from(0xABCDu32),
            core.read_core_reg(RegisterId(3)).unwrap()
        );
    }

    #[test]
    fn armv8r_read_cpsr() {
        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);
        // Save r0, read DSPSR into r0, read r0.
        add_get_reg(&mut probe, 0, 0);
        add_execute(&mut probe, build_mrc(15, 3, 0, 4, 5, 0));
        add_get_reg(&mut probe, 0, 0x6000_01DA);

        let mut state = CortexARState::new();
        let mut core = new_core(probe, &mut state);

        assert_eq!(
            RegisterValue::from(0x6000_01DAu32),
            core.read_core_reg(RegisterId(REG_CPSR)).unwrap()
        );
    }

    #[test]
    fn armv8r_cpsr_writeback_goes_to_dspsr() {
        const NEW_CPSR: u32 = 0x2000_01D3;

        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);

        // run(): CPSR loaded into r0 and moved to DSPSR.
        add_set_reg(&mut probe, 0, NEW_CPSR);
        add_execute(&mut probe, build_mcr(15, 3, 0, 4, 5, 0));
        add_resume(&mut probe);

        let mut state = CortexARState::new();
        let mut core = new_core(probe, &mut state);

        core.write_core_reg(RegisterId(REG_CPSR), NEW_CPSR.into())
            .unwrap();
        core.run().unwrap();
    }

    #[test]
    fn armv8r_fast_read_primes_with_swapped_mcr() {
        const ADDRESS: u32 = 0x1000_0000;

        let mut probe = MockProbe::new(TEST_BASE_ADDRESS);
        add_status(&mut probe, true);

        // Save r0 and r1, load the address into r0.
        add_get_reg(&mut probe, 0, 0);
        add_get_reg(&mut probe, 1, 0);
        add_set_reg(&mut probe, 0, ADDRESS);

        // Prime: MCR p14, 0, R0, c0, c5, 0, halfword-swapped, then wait for TXfull.
        probe.expected_write(
            editr_address(),
            prep_instr_for_itr_32(build_mcr(14, 0, 0, 0, 5, 0)),
        );
        let mut txfull = Edscr(0);
        txfull.set_txfull(true);
        probe.expected_read(edscr_address(), txfull.into());

        // Memory access mode on, discard the primed value, read word 0.
        probe.expected_read(edscr_address(), txfull.into());
        let mut ma = txfull;
        ma.set_ma(true);
        probe.expected_write(edscr_address(), ma.into());
        probe.expected_read(dbgdtrtx_address(), ADDRESS);
        probe.expected_read(dbgdtrtx_address(), 0x1111_1111);

        // Memory access mode off, read the last word, check for errors.
        probe.expected_read(edscr_address(), ma.into());
        probe.expected_write(edscr_address(), txfull.into());
        probe.expected_read(dbgdtrtx_address(), 0x2222_2222);
        probe.expected_read(edscr_address(), Edscr(0).into());

        let mut state = CortexARState::new();
        let mut core = new_core(probe, &mut state);

        let mut data = [0u32; 2];
        core.read_32(ADDRESS.into(), &mut data).unwrap();
        assert_eq!(data, [0x1111_1111, 0x2222_2222]);
    }

    /// Returns the (prefix, aligned, suffix) sizes for an access of `len` bytes at `address`.
    fn split_sizes(address: u64, len: usize) -> (usize, usize, usize) {
        let (prefix, aligned) = Armv8r::aligned_to_32_split_offset(address, len);
        (prefix, aligned, len - prefix - aligned)
    }

    #[test]
    fn armv8r_aligned_split() {
        assert_eq!(split_sizes(0x1000, 8), (0, 8, 0));
        assert_eq!(split_sizes(0x1000, 6), (0, 4, 2));
        assert_eq!(split_sizes(0x1000, 3), (0, 0, 3));
        assert_eq!(split_sizes(0x1001, 8), (3, 4, 1));
        // Within one word: all prefix, nothing aligned.
        assert_eq!(split_sizes(0x1001, 2), (2, 0, 0));
        assert_eq!(split_sizes(0x1003, 1), (1, 0, 0));
    }
}
