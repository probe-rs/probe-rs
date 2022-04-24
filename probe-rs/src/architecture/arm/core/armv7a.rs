//! Register types and the core interface for armv7-a

use crate::architecture::arm::core::register;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::core::RegisterFile;
use crate::error::Error;
use crate::memory::Memory;
use crate::CoreInterface;
use crate::CoreRegisterAddress;
use crate::CoreStatus;
use crate::DebugProbeError;
use crate::HaltReason;
use crate::MemoryInterface;
use crate::{Architecture, CoreInformation};
use anyhow::Result;

use super::State;
use super::ARM_REGISTER_FILE;

use bitfield::bitfield;

use std::mem::size_of;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

/// Errors for the ARMv7-A state machine
#[derive(thiserror::Error, Debug)]
pub enum Armv7aError {
    /// Invalid register number
    #[error("Register number {0} is not valid for ARMv7-A")]
    InvalidRegisterNumber(u16),

    /// Not halted
    #[error("Core is running but operation requires it to be halted")]
    NotHalted,
}

/// Interface for interacting with an ARMv7-A core
pub struct Armv7a<'probe> {
    memory: Memory<'probe>,

    state: &'probe mut State,

    base_address: u32,

    sequence: Arc<dyn ArmDebugSequence>,

    num_breakpoints: Option<u32>,

    itr_enabled: bool,

    register_cache: [Option<(u32, bool)>; 16],
}

impl<'probe> Armv7a<'probe> {
    pub(crate) fn new(
        mut memory: Memory<'probe>,
        state: &'probe mut State,
        base_address: u32,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let address = Dbgdscr::get_mmio_address(base_address);
            let dbgdscr = Dbgdscr(memory.read_word_32(address)?);

            log::debug!("State when connecting: {:x?}", dbgdscr);

            let core_state = if dbgdscr.halted() {
                let reason = dbgdscr.halt_reason();

                log::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            state.current_state = core_state;
            state.initialize();
        }

        Ok(Self {
            memory,
            state,
            base_address,
            sequence,
            num_breakpoints: None,
            itr_enabled: false,
            register_cache: [None; 16],
        })
    }

    /// Execute an instruction
    fn execute_instruction(&mut self, instruction: u32) -> Result<(), Error> {
        if !self.state.current_state.is_halted() {
            return Err(Error::architecture_specific(Armv7aError::NotHalted));
        }

        // Enable ITR if needed
        if !self.itr_enabled {
            let address = Dbgdscr::get_mmio_address(self.base_address);
            let mut dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            dbgdscr.set_itren(true);

            self.memory.write_word_32(address, dbgdscr.into())?;

            self.itr_enabled = true;
        }

        // Run instruction
        let address = Dbgitr::get_mmio_address(self.base_address);
        self.memory.write_word_32(address, instruction)?;

        // Wait for completion
        let address = Dbgdscr::get_mmio_address(self.base_address);
        loop {
            let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            if dbgdscr.instrcoml_l() {
                break;
            }
        }

        Ok(())
    }

    /// Execute an instruction on the CPU and return the result
    fn execute_instruction_with_result(&mut self, instruction: u32) -> Result<u32, Error> {
        // Run instruction
        self.execute_instruction(instruction)?;

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

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn reset_register_cache(&mut self) {
        self.register_cache = [None; 16];
    }

    /// Sync any updated registers back to the core
    fn writeback_registers(&mut self) -> Result<(), Error> {
        for i in 0..self.register_cache.len() {
            if let Some((val, writeback)) = self.register_cache[i] {
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

                            // BX r0
                            let instruction = build_bx(0);
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
}

impl<'probe> CoreInterface for Armv7a<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        let address = Dbgdscr::get_mmio_address(self.base_address);

        while start.elapsed() < timeout {
            let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            if dbgdscr.halted() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        let address = Dbgdscr::get_mmio_address(self.base_address);
        let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);

        Ok(dbgdscr.halted())
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        let address = Dbgdrcr::get_mmio_address(self.base_address);
        let mut value = Dbgdrcr(0);
        value.set_hrq(true);

        self.memory.write_word_32(address, value.into())?;

        self.wait_for_core_halted(timeout)?;

        // Reset our cached values
        self.reset_register_cache();

        // Update core status
        let _ = self.status()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn run(&mut self) -> Result<(), Error> {
        // set writeback values
        self.writeback_registers()?;

        let address = Dbgdrcr::get_mmio_address(self.base_address);
        let mut value = Dbgdrcr(0);
        value.set_rrq(true);

        self.memory.write_word_32(address, value.into())?;

        // Wait for ack
        let address = Dbgdscr::get_mmio_address(self.base_address);

        loop {
            let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            if dbgdscr.restarted() {
                break;
            }
        }

        // Recompute / verify current state
        self.state.current_state = CoreStatus::Running;
        let _ = self.status()?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(
            &mut self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;

        // Reset our cached values
        self.reset_register_cache();

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_catch_set(
            &mut self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;
        self.sequence.reset_system(
            &mut self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;

        // Request halt
        let address = Dbgdrcr::get_mmio_address(self.base_address);
        let mut value = Dbgdrcr(0);
        value.set_hrq(true);

        self.memory.write_word_32(address, value.into())?;

        // Release from reset
        self.sequence.reset_catch_clear(
            &mut self.memory,
            crate::CoreType::Armv7a,
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
        Ok(CoreInformation { pc: pc_value })
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // Save current breakpoint
        let bp_unit_index = (self.available_breakpoint_units()? - 1) as usize;
        let bp_value_addr =
            Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;
        let saved_bp_value = self.memory.read_word_32(bp_value_addr)?;

        let bp_control_addr =
            Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;
        let saved_bp_control = self.memory.read_word_32(bp_control_addr)?;

        // Set breakpoint for any change
        let current_pc = self.read_core_reg(register::PC.address)?;
        let mut bp_control = Dbgbcr(0);

        // Breakpoint type - address mismatch
        bp_control.set_bt(0b0100);
        // Match on all modes
        bp_control.set_hmc(true);
        bp_control.set_pmc(0b11);
        // Match on all bytes
        bp_control.set_bas(0b1111);
        // Enable
        bp_control.set_e(true);

        self.memory.write_word_32(bp_value_addr, current_pc)?;
        self.memory
            .write_word_32(bp_control_addr, bp_control.into())?;

        // Resume
        self.run()?;

        // Wait for halt
        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Reset breakpoint
        self.memory.write_word_32(bp_value_addr, saved_bp_value)?;
        self.memory
            .write_word_32(bp_control_addr, saved_bp_control)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, Error> {
        let reg_num = address.0;

        // check cache
        if (reg_num as usize) < self.register_cache.len() {
            if let Some(cached_result) = self.register_cache[reg_num as usize] {
                return Ok(cached_result.0);
            }
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

                // cache r0 since we're going to clobber it
                let r0_val = self.read_core_reg(CoreRegisterAddress(0))?;

                // Mark r0 as needing writeback
                self.register_cache[0] = Some((r0_val, true));

                // MOV r0, PC
                let instruction = build_mov(0, 15);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let pra_plus_offset = self.execute_instruction_with_result(instruction)?;

                // PC returned is PC + 8
                Ok(pra_plus_offset - 8)
            }
            _ => Err(Error::architecture_specific(
                Armv7aError::InvalidRegisterNumber(reg_num),
            )),
        };

        if let Ok(value) = result {
            self.register_cache[reg_num as usize] = Some((value, false));
        }

        result
    }

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()> {
        let reg_num = address.0;

        if (reg_num as usize) >= self.register_cache.len() {
            return Err(
                Error::architecture_specific(Armv7aError::InvalidRegisterNumber(reg_num)).into(),
            );
        }
        self.register_cache[reg_num as usize] = Some((value, true));

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        if self.num_breakpoints.is_none() {
            let address = Dbgdidr::get_mmio_address(self.base_address);
            let dbgdidr = Dbgdidr(self.memory.read_word_32(address)?);

            self.num_breakpoints = Some(dbgdidr.brps() + 1);
        }
        Ok(self.num_breakpoints.unwrap())
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        // Breakpoints are always on with v7-A
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;
        let bp_control_addr =
            Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;
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

        self.memory.write_word_32(bp_value_addr, addr)?;
        self.memory
            .write_word_32(bp_control_addr, bp_control.into())?;

        Ok(())
    }

    fn registers(&self) -> &'static RegisterFile {
        &ARM_REGISTER_FILE
    }

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let bp_value_addr =
            Dbgbvr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;
        let bp_control_addr =
            Dbgbcr::get_mmio_address(self.base_address) + (bp_unit_index * size_of::<u32>()) as u32;

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

    fn status(&mut self) -> Result<crate::core::CoreStatus, Error> {
        // determine current state
        let address = Dbgdscr::get_mmio_address(self.base_address);
        let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);

        if dbgdscr.halted() {
            let reason = dbgdscr.halt_reason();

            self.state.current_state = CoreStatus::Halted(reason);

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
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u32>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;

        for bp_unit_index in 0..num_hw_breakpoints {
            let bp_value_addr = Dbgbvr::get_mmio_address(self.base_address)
                + (bp_unit_index * size_of::<u32>()) as u32;
            let bp_value = self.memory.read_word_32(bp_value_addr)?;

            let bp_control_addr = Dbgbcr::get_mmio_address(self.base_address)
                + (bp_unit_index * size_of::<u32>()) as u32;
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

impl<'probe> MemoryInterface for Armv7a<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        self.memory.read_word_32(address)
    }
    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        self.memory.read_word_8(address)
    }
    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        self.memory.read_32(address, data)
    }
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.memory.read_8(address, data)
    }
    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        self.memory.write_word_32(address, data)
    }
    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        self.memory.write_word_8(address, data)
    }
    fn write_32(&mut self, address: u32, data: &[u32]) -> Result<(), Error> {
        self.memory.write_32(address, data)
    }
    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), Error> {
        self.memory.write_8(address, data)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.memory.flush()
    }
}

// Debug register definitions

/// A debug register that is accessible to the external debugger
pub trait Armv7DebugRegister {
    /// Register number
    const NUMBER: u32;

    /// The register's name.
    const NAME: &'static str;

    /// Get the address in the memory map
    fn get_mmio_address(base_address: u32) -> u32 {
        base_address + (Self::NUMBER * size_of::<u32>() as u32)
    }
}

bitfield! {
    /// DBGDSCR - Debug Status and Control Registers
    #[derive(Copy, Clone)]
    pub struct Dbgdscr(u32);
    impl Debug;

    /// DBGDTRRX register full. The possible values of this bit are:
    ///
    /// 0
    /// DBGDTRRX register empty.
    ///
    /// 1
    /// DBGDTRRX register full.
    pub rxfull, _: 30;

    /// DBGDTRTX register full. The possible values of this bit are:
    /// 0
    /// DBGDTRTX register empty.
    ///
    /// 1
    /// DBGDTRTX register full.
    pub txfull, _: 29;

    /// Latched RXfull. This controls the behavior of the processor on writes to DBGDTRRXext.
    pub rxfull_l, _: 27;

    /// Latched TXfull. This controls the behavior of the processor on reads of DBGDTRTXext.
    pub txfull_l, _: 26;

    /// Sticky Pipeline Advance bit. This bit is set to 1 whenever the processor pipeline advances by retiring one or more instructions. It is cleared to 0 only by a write to DBGDRCR.CSPA.
    pub pipeadv, _: 25;

    /// Latched Instruction Complete. This is a copy of the internal InstrCompl flag, taken on each read of DBGDSCRext. InstrCompl signals whether the processor has completed execution of an instruction issued through DBGITR. InstrCompl is not visible directly in any register.
    ///
    /// On a read of DBGDSCRext when the processor is in Debug state, InstrCompl_l always returns the current value of InstrCompl. The meanings of the values of InstrCompl_l are:
    ///
    /// 0
    /// An instruction previously issued through the DBGITR has not completed its changes to the architectural state of the processor.
    ///
    /// 1
    /// All instructions previously issued through the DBGITR have completed their changes to the architectural state of the processor.
    pub instrcoml_l, set_instrcoml_l: 24;

    /// External DCC access mode. This field controls the access mode for the external views of the DCC registers and the DBGITR. Possible values are:
    ///
    /// 0b00
    /// Non-blocking mode.
    ///
    /// 0b01
    /// Stall mode.
    ///
    /// 0b10
    /// Fast mode.
    ///
    /// The value 0b11 is reserved.
    pub extdccmode, _: 21, 20;

    /// Asynchronous Aborts Discarded. The possible values of this bit are:
    ///
    /// 0
    /// Asynchronous aborts handled normally.
    ///
    /// 1
    /// On an asynchronous abort to which this bit applies, the processor sets the Sticky Asynchronous Abort bit, ADABORT_l, to 1 but otherwise discards the abort.
    pub adadiscard, _: 19;

    /// Non-secure state status. If the implementation includes the Security Extensions, this bit indicates whether the processor is in the Secure state. The possible values of this bit are:
    ///
    /// 0
    /// The processor is in the Secure state.
    ///
    /// 1
    /// The processor is in the Non-secure state.
    pub ns, _: 18;

    /// Secure PL1 Non-Invasive Debug Disabled. This bit shows if non-invasive debug is permitted in Secure PL1 modes. The possible values of the bit are:
    ///
    /// 0
    /// Non-invasive debug is permitted in Secure PL1 modes.
    ///
    /// 1
    /// Non-invasive debug is not permitted in Secure PL1 modes.
    pub spniddis, _: 17;

    /// Secure PL1 Invasive Debug Disabled bit. This bit shows if invasive debug is permitted in Secure PL1 modes. The possible values of the bit are:
    ///
    /// 0
    /// Invasive debug is permitted in Secure PL1 modes.
    ///
    /// 1
    /// Invasive debug is not permitted in Secure PL1 modes.
    pub spiddis, _: 16;

    /// Monitor debug-mode enable. The possible values of this bit are:
    ///
    /// 0
    /// Monitor debug-mode disabled.
    ///
    /// 1
    /// Monitor debug-mode enabled.
    pub mdbgen, set_mdbgen: 15;

    ///Halting debug-mode enable. The possible values of this bit are:
    ///
    /// 0
    /// Halting debug-mode disabled.
    ///
    /// 1
    /// Halting debug-mode enabled.
    pub hdbgen, set_hdbgen: 14;

    /// Execute ARM instruction enable. This bit enables the execution of ARM instructions through the DBGITR. The possible values of this bit are:
    ///
    /// 0
    /// ITR mechanism disabled.
    ///
    /// 1
    /// The ITR mechanism for forcing the processor to execute instructions in Debug state via the external debug interface is enabled.
    pub itren, set_itren: 13;

    /// User mode access to Debug Communications Channel (DCC) disable. The possible values of this bit are:
    ///
    /// 0
    /// User mode access to DCC enabled.
    ///
    /// 1
    /// User mode access to DCC disabled.
    pub udccdis, set_udccdis: 12;

    /// Interrupts Disable. Setting this bit to 1 masks the taking of IRQs and FIQs. The possible values of this bit are:
    ///
    /// 0
    /// Interrupts enabled.
    ///
    /// 1
    /// Interrupts disabled.
    pub intdis, set_intdis: 11;

    /// Force Debug Acknowledge. A debugger can use this bit to force any implemented debug acknowledge output signals to be asserted. The possible values of this bit are:
    ///
    /// 0
    /// Debug acknowledge signals under normal processor control.
    ///
    /// 1
    /// Debug acknowledge signals asserted, regardless of the processor state.
    pub dbgack, set_dbgack: 10;

    /// Fault status. This bit is updated on every Data Abort exception generated in Debug state, and might indicate that the exception syndrome information was written to the PL2 exception syndrome registers. The possible values are:
    ///
    /// 0
    /// Software must use the current state and mode and the value of HCR.TGE to determine which of the following sets of registers holds information about the Data Abort exception:
    ///
    /// The PL1 fault reporting registers, meaning the DFSR and DFAR, and the ADFSR if it is implemented.
    /// The PL2 fault syndrome registers, meaning the HSR, HDFAR, and HPFAR, and the HADFSR if it is implemented.
    /// 1
    /// Fault status information was written to the PL2 fault syndrome registers.
    pub fs, _: 9;

    /// Sticky Undefined Instruction. This bit is set to 1 by any Undefined Instruction exceptions generated by instructions issued to the processor while in Debug state. The possible values of this bit are:
    ///
    /// 0
    /// No Undefined Instruction exception has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// An Undefined Instruction exception has been generated since the last time this bit was cleared to 0.
    pub und_l, _: 8;

    /// Sticky Asynchronous Abort. When the ADAdiscard bit, bit[19], is set to 1, ADABORT_l is set to 1 by any asynchronous abort that occurs when the processor is in Debug state.
    ///
    /// The possible values of this bit are:
    ///
    /// 0
    /// No asynchronous abort has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// Since the last time this bit was cleared to 0, an asynchronous abort has been generated while ADAdiscard was set to 1.
    pub adabort_l, _: 7;

    /// Sticky Synchronous Data Abort. This bit is set to 1 by any Data Abort exception that is generated synchronously when the processor is in Debug state. The possible values of this bit are:
    ///
    /// 0
    /// No synchronous Data Abort exception has been generated since the last time this bit was cleared to 0.
    ///
    /// 1
    /// A synchronous Data Abort exception has been generated since the last time this bit was cleared to 0.
    pub sdabort_l, _: 6;

    /// Method of Debug entry.
    pub moe, _: 5, 2;

    /// Processor Restarted. The possible values of this bit are:
    ///
    /// 0
    /// The processor is exiting Debug state. This bit only reads as 0 between receiving a restart request, and restarting Non-debug state operation.
    ///
    /// 1
    /// The processor has exited Debug state. This bit remains set to 1 if the processor re-enters Debug state.
    pub restarted, set_restarted: 1;

    /// Processor Halted. The possible values of this bit are:
    ///
    /// 0
    /// The processor is in Non-debug state.
    ///
    /// 1
    /// The processor is in Debug state.
    pub halted, set_halted: 0;
}

impl Dbgdscr {
    /// Decode the MOE register into HaltReason
    fn halt_reason(&self) -> HaltReason {
        if self.halted() {
            match self.moe() {
                // Halt request from debugger
                0b0000 => HaltReason::Request,
                // Breakpoint debug event
                0b0001 => HaltReason::Breakpoint,
                // Async watchpoint debug event
                0b0010 => HaltReason::Watchpoint,
                // BKPT instruction
                0b0011 => HaltReason::Breakpoint,
                // External halt request
                0b0100 => HaltReason::External,
                // Vector catch
                0b0101 => HaltReason::Exception,
                // OS Unlock vector catch
                0b1000 => HaltReason::Exception,
                // Sync watchpoint debug event
                0b1010 => HaltReason::Breakpoint,
                // All other values are reserved
                _ => HaltReason::Unknown,
            }
        } else {
            // Not halted or cannot detect
            HaltReason::Unknown
        }
    }
}

impl Armv7DebugRegister for Dbgdscr {
    const NUMBER: u32 = 34;
    const NAME: &'static str = "DBGDSCR";
}

impl From<u32> for Dbgdscr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdscr> for u32 {
    fn from(value: Dbgdscr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDIDR - Debug ID Register
    #[derive(Copy, Clone)]
    pub struct Dbgdidr(u32);
    impl Debug;

    /// The number of watchpoints implemented. The number of implemented watchpoints is one more than the value of this field.
    pub wrps, _: 31, 28;

    /// The number of breakpoints implemented. The number of implemented breakpoints is one more than value of this field.
    pub brps, set_brps: 27, 24;

    /// The number of breakpoints that can be used for Context matching. This is one more than the value of this field.
    pub ctx_cmps, _: 23, 20;

    /// The Debug architecture version. The permitted values of this field are:
    ///
    /// 0b0001
    /// ARMv6, v6 Debug architecture.
    ///
    /// 0b0010
    /// ARMv6, v6.1 Debug architecture.
    ///
    /// 0b0011
    /// ARMv7, v7 Debug architecture, with all CP14 registers implemented.
    ///
    /// 0b0100
    /// ARMv7, v7 Debug architecture, with only the baseline CP14 registers implemented.
    ///
    /// 0b0101
    /// ARMv7, v7.1 Debug architecture.
    ///
    /// All other values are reserved.
    pub version, _: 19, 16;

    /// Debug Device ID Register, DBGDEVID, implemented.
    pub devid_imp, _: 15;

    /// Secure User halting debug not implemented
    pub nsuhd_imp, _: 14;

    /// Program Counter Sampling Register, DBGPCSR, implemented as register 33.
    pub pcsr_imp, _: 13;

    /// Security Extensions implemented.
    pub se_imp, _: 12;

    /// This field holds an implementation defined variant number.
    pub variant, _: 7, 4;

    /// This field holds an implementation defined revision number.
    pub revision, _: 3, 0;
}

impl Armv7DebugRegister for Dbgdidr {
    const NUMBER: u32 = 0;
    const NAME: &'static str = "DBGDIDR";
}

impl From<u32> for Dbgdidr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdidr> for u32 {
    fn from(value: Dbgdidr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDRCR - Debug Run Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgdrcr(u32);
    impl Debug;

    /// Cancel Bus Requests Request
    pub cbrrq, set_cbrrq: 4;

    /// Clear Sticky Pipeline Advance
    pub cspa, set_cspa: 3;

    /// Clear Sticky Exceptions
    pub cse, set_cse: 2;

    /// Restart request
    pub rrq, set_rrq: 1;

    /// Halt request
    pub hrq, set_hrq: 0;
}

impl Armv7DebugRegister for Dbgdrcr {
    const NUMBER: u32 = 36;
    const NAME: &'static str = "DBGDRCR";
}

impl From<u32> for Dbgdrcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdrcr> for u32 {
    fn from(value: Dbgdrcr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGBVR - Breakpoint Value Register
    #[derive(Copy, Clone)]
    pub struct Dbgbvr(u32);
    impl Debug;

    /// Breakpoint address
    pub value, set_value : 31, 0;
}

impl Armv7DebugRegister for Dbgbvr {
    const NUMBER: u32 = 64;
    const NAME: &'static str = "DBGBVR";
}

impl From<u32> for Dbgbvr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgbvr> for u32 {
    fn from(value: Dbgbvr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGBCR - Breakpoint Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgbcr(u32);
    impl Debug;

    /// Address range mask. Whether masking is supported is implementation defined.
    pub mask, set_mask : 28, 24;

    /// Breakpoint type
    pub bt, set_bt : 23, 20;

    /// Linked breakpoint number
    pub lbn, set_lbn : 19, 16;

    /// Security state control
    pub ssc, set_ssc : 15, 14;

    /// Hyp mode control bit
    pub hmc, set_hmc: 13;

    /// Byte address select
    pub bas, set_bas: 8, 5;

    /// Privileged mode control
    pub pmc, set_pmc: 2, 1;

    /// Breakpoint enable
    pub e, set_e: 0;
}

impl Armv7DebugRegister for Dbgbcr {
    const NUMBER: u32 = 80;
    const NAME: &'static str = "DBGBCR";
}

impl From<u32> for Dbgbcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgbcr> for u32 {
    fn from(value: Dbgbcr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGLAR - Lock Access Register
    #[derive(Copy, Clone)]
    pub struct Dbglar(u32);
    impl Debug;

    /// Lock value
    pub value, set_value : 31, 0;

}

impl Armv7DebugRegister for Dbglar {
    const NUMBER: u32 = 1004;
    const NAME: &'static str = "DBGLAR";
}

impl From<u32> for Dbglar {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbglar> for u32 {
    fn from(value: Dbglar) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDSCCR - State Cache Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgdsccr(u32);
    impl Debug;

    /// Force Write-Through
    pub nwt, set_nwt: 2;

    /// Instruction cache
    pub nil, set_nil: 1;

    /// Data or unified cache.
    pub ndl, set_ndl: 0;
}

impl Armv7DebugRegister for Dbgdsccr {
    const NUMBER: u32 = 10;
    const NAME: &'static str = "DBGDSCCR";
}

impl From<u32> for Dbgdsccr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdsccr> for u32 {
    fn from(value: Dbgdsccr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDSMCR - Debug State MMU Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgdsmcr(u32);
    impl Debug;

    /// Instruction TLB matching bit
    pub nium, set_nium: 3;

    /// Data or Unified TLB matching bit
    pub ndum, set_ndum: 2;

    /// Instruction TLB loading bit
    pub niul, set_niul: 1;

    /// Data or Unified TLB loading bit
    pub ndul, set_ndul: 0;
}

impl Armv7DebugRegister for Dbgdsmcr {
    const NUMBER: u32 = 11;
    const NAME: &'static str = "DBGDSMCR";
}

impl From<u32> for Dbgdsmcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdsmcr> for u32 {
    fn from(value: Dbgdsmcr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGITR - Instruction Transfer Register
    #[derive(Copy, Clone)]
    pub struct Dbgitr(u32);
    impl Debug;

    /// Instruction value
    pub value, set_value: 31, 0;
}

impl Armv7DebugRegister for Dbgitr {
    const NUMBER: u32 = 33;
    const NAME: &'static str = "DBGITR";
}

impl From<u32> for Dbgitr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgitr> for u32 {
    fn from(value: Dbgitr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDTRTX - Target to Host data transfer register
    #[derive(Copy, Clone)]
    pub struct Dbgdtrtx(u32);
    impl Debug;

    /// Value
    pub value, set_value: 31, 0;
}

impl Armv7DebugRegister for Dbgdtrtx {
    const NUMBER: u32 = 35;
    const NAME: &'static str = "DBGDTRTX";
}

impl From<u32> for Dbgdtrtx {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdtrtx> for u32 {
    fn from(value: Dbgdtrtx) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGDTRRX - Host to Target data transfer register
    #[derive(Copy, Clone)]
    pub struct Dbgdtrrx(u32);
    impl Debug;

    /// Value
    pub value, set_value: 31, 0;
}

impl Armv7DebugRegister for Dbgdtrrx {
    const NUMBER: u32 = 32;
    const NAME: &'static str = "DBGDTRRX";
}

impl From<u32> for Dbgdtrrx {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgdtrrx> for u32 {
    fn from(value: Dbgdtrrx) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGPRCR - Powerdown and Reset Control Register
    #[derive(Copy, Clone)]
    pub struct Dbgprcr(u32);
    impl Debug;

    /// Core powerup request
    pub corepurq, set_corepurq : 3;

    /// Hold core in warm reset
    pub hcwr, set_hcwr : 2;

    /// Core warm reset request
    pub cwrr, set_cwrr : 1;

    /// Core no powerdown request
    pub corenpdrq, set_corenpdrq : 0;
}

impl Armv7DebugRegister for Dbgprcr {
    const NUMBER: u32 = 196;
    const NAME: &'static str = "DBGPRCR";
}

impl From<u32> for Dbgprcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgprcr> for u32 {
    fn from(value: Dbgprcr) -> Self {
        value.0
    }
}

bitfield! {
    /// DBGPRSR - Powerdown and Reset Status Register
    #[derive(Copy, Clone)]
    pub struct Dbgprsr(u32);
    impl Debug;

    /// OS Double Lock Status
    pub dlk, _ : 6;

    /// OS Lock Status
    pub oslk, _ : 5;

    /// Halted
    pub halted, _ : 4;

    /// Stick reset status
    pub sr, _ : 3;

    /// Reset status
    pub r, _ : 2;

    /// Stick power down status
    pub spd, _ : 1;

    /// Power up status
    pub pu, _ : 0;
}

impl Armv7DebugRegister for Dbgprsr {
    const NUMBER: u32 = 197;
    const NAME: &'static str = "DBGPRSR";
}

impl From<u32> for Dbgprsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dbgprsr> for u32 {
    fn from(value: Dbgprsr) -> Self {
        value.0
    }
}

/// Build a MOV insturction
fn build_mov(rd: u16, rm: u16) -> u32 {
    let mut ret = 0b1110_0001_1010_0000_0000_0000_0000_0000;

    ret |= (rd as u32) << 12;
    ret |= rm as u32;

    ret
}

/// Build a MCR instruction
fn build_mcr(
    coproc: u8,
    opcode1: u8,
    reg: u16,
    ctrl_reg_n: u8,
    ctrl_reg_m: u8,
    opcode2: u8,
) -> u32 {
    let mut ret = 0b1110_1110_0000_0000_0000_0000_0001_0000;

    ret |= (coproc as u32) << 8;
    ret |= (opcode1 as u32) << 21;
    ret |= (reg as u32) << 12;
    ret |= (ctrl_reg_n as u32) << 16;
    ret |= ctrl_reg_m as u32;
    ret |= (opcode2 as u32) << 5;

    ret
}

fn build_mrc(
    coproc: u8,
    opcode1: u8,
    reg: u16,
    ctrl_reg_n: u8,
    ctrl_reg_m: u8,
    opcode2: u8,
) -> u32 {
    let mut ret = 0b1110_1110_0001_0000_0000_0000_0001_0000;

    ret |= (coproc as u32) << 8;
    ret |= (opcode1 as u32) << 21;
    ret |= (reg as u32) << 12;
    ret |= (ctrl_reg_n as u32) << 16;
    ret |= ctrl_reg_m as u32;
    ret |= (opcode2 as u32) << 5;

    ret
}

fn build_bx(reg: u16) -> u32 {
    let mut ret = 0b1110_0001_0010_1111_1111_1111_0001_0000;

    ret |= reg as u32;

    ret
}

#[cfg(test)]
mod test {
    use crate::architecture::arm::{
        ap::MemoryAp, communication_interface::SwdSequence,
        memory::adi_v5_memory_interface::ArmProbe, sequences::DefaultArmSequence, ApAddress,
        DpAddress,
    };

    use super::*;

    const TEST_BASE_ADDRESS: u32 = 0x8000_1000;

    fn address_to_reg_num(address: u32) -> u32 {
        (address - TEST_BASE_ADDRESS) / 4
    }

    pub struct ExpectedMemoryOp {
        read: bool,
        address: u32,
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

        pub fn expected_read(&mut self, addr: u32, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: true,
                address: addr,
                value: value,
            });
        }

        pub fn expected_write(&mut self, addr: u32, value: u32) {
            self.expected_ops.push(ExpectedMemoryOp {
                read: false,
                address: addr,
                value: value,
            });
        }
    }

    impl ArmProbe for MockProbe {
        fn read_core_reg(
            &mut self,
            _ap: MemoryAp,
            _addr: CoreRegisterAddress,
        ) -> Result<u32, Error> {
            todo!()
        }

        fn write_core_reg(
            &mut self,
            _ap: MemoryAp,
            _addr: CoreRegisterAddress,
            _value: u32,
        ) -> Result<(), Error> {
            todo!()
        }

        fn read_8(&mut self, _ap: MemoryAp, _address: u32, _data: &mut [u8]) -> Result<(), Error> {
            todo!()
        }

        fn read_32(&mut self, _ap: MemoryAp, address: u32, data: &mut [u32]) -> Result<(), Error> {
            if self.expected_ops.len() == 0 {
                panic!(
                    "Received unexpected read_32 op: register {:#}",
                    address_to_reg_num(address)
                );
            }

            assert_eq!(data.len(), 1);

            let expected_op = self.expected_ops.remove(0);

            assert_eq!(expected_op.read, true);
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

        fn write_8(&mut self, _ap: MemoryAp, _address: u32, _data: &[u8]) -> Result<(), Error> {
            todo!()
        }

        fn write_32(&mut self, _ap: MemoryAp, address: u32, data: &[u32]) -> Result<(), Error> {
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
                "Read from unexpected register: Expected {:#} Actual: {:#}",
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
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(halted);
        dbgdscr.set_restarted(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());
    }

    fn add_enable_itr_expectations(probe: &mut MockProbe) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());
        dbgdscr.set_itren(true);
        probe.expected_write(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());
    }

    fn add_read_reg_expectations(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(
            Dbgitr::get_mmio_address(TEST_BASE_ADDRESS),
            build_mcr(14, 0, reg, 0, 5, 0),
        );
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());
        probe.expected_read(Dbgdtrtx::get_mmio_address(TEST_BASE_ADDRESS), value);
    }

    fn add_read_pc_expectations(probe: &mut MockProbe, value: u32) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);

        probe.expected_write(
            Dbgitr::get_mmio_address(TEST_BASE_ADDRESS),
            build_mov(0, 15),
        );
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());
        // + 8 to add expected offset on halt
        add_read_reg_expectations(probe, 0, value + 8);
    }

    fn add_idr_expectations(probe: &mut MockProbe, bp_count: u32) {
        let mut dbgdidr = Dbgdidr(0);
        dbgdidr.set_brps(bp_count - 1);
        probe.expected_read(Dbgdidr::get_mmio_address(TEST_BASE_ADDRESS), dbgdidr.into());
    }

    #[test]
    fn armv7a_new() {
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

        let _ = Armv7a::new(
            mock_mem,
            &mut State::new(),
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();
    }

    #[test]
    fn armv7a_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        dbgdscr.set_halted(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read false, second read true
        assert_eq!(false, armv7a.core_halted().unwrap());
        assert_eq!(true, armv7a.core_halted().unwrap());
    }

    #[test]
    fn armv7a_wait_for_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        dbgdscr.set_halted(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Should halt on second read
        armv7a
            .wait_for_core_halted(Duration::from_millis(100))
            .unwrap();
    }

    #[test]
    fn armv7a_status_running() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Should halt on second read
        assert_eq!(CoreStatus::Running, armv7a.status().unwrap());
    }

    #[test]
    fn armv7a_status_halted() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(true);
        probe.expected_read(Dbgdscr::get_mmio_address(TEST_BASE_ADDRESS), dbgdscr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Should halt on second read
        assert_eq!(
            CoreStatus::Halted(HaltReason::Request),
            armv7a.status().unwrap()
        );
    }

    #[test]
    fn armv7a_read_core_reg_common() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read status, update ITR
        add_enable_itr_expectations(&mut probe);

        // Read register
        add_read_reg_expectations(&mut probe, 2, REG_VALUE);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            REG_VALUE,
            armv7a.read_core_reg(CoreRegisterAddress(2)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            REG_VALUE,
            armv7a.read_core_reg(CoreRegisterAddress(2)).unwrap()
        );
    }

    #[test]
    fn armv7a_read_core_reg_pc() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read status, update ITR
        add_enable_itr_expectations(&mut probe);

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

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            REG_VALUE,
            armv7a.read_core_reg(CoreRegisterAddress(15)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            REG_VALUE,
            armv7a.read_core_reg(CoreRegisterAddress(15)).unwrap()
        );
    }

    #[test]
    fn armv7a_halt() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Write halt request
        let mut dbgdrcr = Dbgdrcr(0);
        dbgdrcr.set_hrq(true);
        probe.expected_write(Dbgdrcr::get_mmio_address(TEST_BASE_ADDRESS), dbgdrcr.into());

        // Wait for halted
        add_status_expectations(&mut probe, true);

        // Read status
        add_status_expectations(&mut probe, true);

        // Read status, update ITR
        add_enable_itr_expectations(&mut probe);

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

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        // Verify PC
        assert_eq!(
            REG_VALUE,
            armv7a.halt(Duration::from_millis(100)).unwrap().pc
        );
    }

    #[test]
    fn armv7a_run() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Write resume request
        let mut dbgdrcr = Dbgdrcr(0);
        dbgdrcr.set_rrq(true);
        probe.expected_write(Dbgdrcr::get_mmio_address(TEST_BASE_ADDRESS), dbgdrcr.into());

        // Wait for running
        add_status_expectations(&mut probe, false);

        // Read status
        add_status_expectations(&mut probe, false);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv7a.run().unwrap();
    }

    #[test]
    fn armv7a_available_breakpoint_units() {
        const BP_COUNT: u32 = 4;
        let mut probe = MockProbe::new();
        let mut state = State::new();

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

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        assert_eq!(BP_COUNT, armv7a.available_breakpoint_units().unwrap());
    }

    #[test]
    fn armv7a_hw_breakpoints() {
        const BP_COUNT: u32 = 4;
        const BP1: u32 = 0x2345;
        const BP2: u32 = 0x8000_0000;
        let mut probe = MockProbe::new();
        let mut state = State::new();

        // Add expectations
        add_status_expectations(&mut probe, true);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        // Read BP values and controls
        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS), BP1);
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS), 1);

        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (1 * 4), BP2);
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (1 * 4), 1);

        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (2 * 4), 0);
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (2 * 4), 0);

        probe.expected_read(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS) + (3 * 4), 0);
        probe.expected_read(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS) + (3 * 4), 0);

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        let results = armv7a.hw_breakpoints().unwrap();
        assert_eq!(Some(BP1), results[0]);
        assert_eq!(Some(BP2), results[1]);
        assert_eq!(None, results[2]);
        assert_eq!(None, results[3]);
    }

    #[test]
    fn armv7a_set_hw_breakpoint() {
        const BP_VALUE: u32 = 0x2345;
        let mut probe = MockProbe::new();
        let mut state = State::new();

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

        probe.expected_write(Dbgbvr::get_mmio_address(TEST_BASE_ADDRESS), BP_VALUE);
        probe.expected_write(Dbgbcr::get_mmio_address(TEST_BASE_ADDRESS), dbgbcr.into());

        let mock_mem = Memory::new(
            probe,
            MemoryAp::new(ApAddress {
                ap: 0,
                dp: DpAddress::Default,
            }),
        );

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv7a.set_hw_breakpoint(0, BP_VALUE).unwrap();
    }

    #[test]
    fn armv7a_clear_hw_breakpoint() {
        let mut probe = MockProbe::new();
        let mut state = State::new();

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

        let mut armv7a = Armv7a::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
        )
        .unwrap();

        armv7a.clear_hw_breakpoint(0).unwrap();
    }

    #[test]
    fn gen_mcr_instruction() {
        let instr = build_mcr(14, 0, 2, 1, 2, 3);

        // MCR p14, 0, r2, c1, c2, 3
        assert_eq!(0xEE012E72, instr);
    }

    #[test]
    fn gen_mrc_instruction() {
        let instr = build_mrc(14, 0, 2, 1, 2, 3);

        // MRC p14, 0, r2, c1, c2, 3
        assert_eq!(0xEE112E72, instr);
    }

    #[test]
    fn gen_mov_instruction() {
        let instr = build_mov(2, 15);

        // MOV r2, pc
        assert_eq!(0xE1A0200F, instr);
    }

    #[test]
    fn gen_bx_instruction() {
        let instr = build_bx(2);

        // BX r2
        assert_eq!(0xE12FFF12, instr);
    }
}
