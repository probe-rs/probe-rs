//! All the interface bits for Xtensa.

use std::time::Duration;

use probe_rs_target::{Architecture, CoreType, InstructionSet};

use crate::{
    architecture::xtensa::{
        arch::{instruction::Instruction, Register, SpecialRegister},
        communication_interface::{DebugCause, IBreakEn},
        registers::{FP, PC, RA, SP, XTENSA_CORE_REGSISTERS},
    },
    core::{
        registers::{CoreRegisters, RegisterId, RegisterValue},
        BreakpointCause,
    },
    semihosting::decode_semihosting_syscall,
    CoreInformation, CoreInterface, CoreRegister, CoreStatus, Error, HaltReason, MemoryInterface,
};

use self::communication_interface::XtensaCommunicationInterface;

pub(crate) mod arch;
mod xdm;

pub mod communication_interface;
pub(crate) mod registers;
pub(crate) mod sequences;

#[derive(Debug)]
/// Flags used to control the [`SpecificCoreState`](crate::core::SpecificCoreState) for Xtensa
/// architecture.
pub struct XtensaState {
    breakpoints_enabled: bool,
    breakpoint_set: [bool; 2],

    /// Whether the PC was written since we last halted. Used to avoid incrementing the PC on
    /// resume.
    pc_written: bool,
}

impl XtensaState {
    /// Creates a new [`XtensaState`].
    pub(crate) fn new() -> Self {
        Self {
            breakpoints_enabled: false,
            breakpoint_set: [false; 2],
            pc_written: false,
        }
    }

    fn breakpoint_mask(&self) -> u32 {
        self.breakpoint_set
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &set)| if set { acc | (1 << i) } else { acc })
    }
}

/// An interface to operate Xtensa cores.
pub struct Xtensa<'probe> {
    interface: &'probe mut XtensaCommunicationInterface,
    state: &'probe mut XtensaState,
}

impl<'probe> Xtensa<'probe> {
    const IBREAKA_REGS: [SpecialRegister; 2] =
        [SpecialRegister::IBreakA0, SpecialRegister::IBreakA1];

    /// Create a new Xtensa interface.
    pub fn new(
        interface: &'probe mut XtensaCommunicationInterface,
        state: &'probe mut XtensaState,
    ) -> Self {
        Self { interface, state }
    }

    fn core_info(&mut self) -> Result<CoreInformation, Error> {
        let pc = self.read_core_reg(self.program_counter().id)?;

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn skip_breakpoint_instruction(&mut self) -> Result<(), Error> {
        if !self.state.pc_written {
            let debug_cause = self.interface.read_register::<DebugCause>()?;

            let pc_increment = if debug_cause.break_instruction() {
                3
            } else if debug_cause.break_n_instruction() {
                2
            } else {
                0
            };

            if pc_increment > 0 {
                // Step through the breakpoint
                let mut pc = self.read_core_reg(self.program_counter().id)?;

                pc.increment_address(pc_increment)?;

                self.write_core_reg(self.program_counter().into(), pc)?;
            }
        }

        Ok(())
    }

    /// Check if the current breakpoint is a semihosting call
    // OpenOCD implementation: https://github.com/espressif/openocd-esp32/blob/93dd01511fd13d4a9fb322cd9b600c337becef9e/src/target/espressif/esp_xtensa_semihosting.c#L42-L103
    fn check_for_semihosting(
        old_reason: HaltReason,
        core: &mut dyn CoreInterface,
    ) -> Result<HaltReason, Error> {
        let mut reason = old_reason;
        let pc: u32 = core.read_core_reg(core.program_counter().id)?.try_into()?;

        let mut actual_instructions = [0u32; 1];
        core.read_32((pc) as u64, &mut actual_instructions)?;
        let actual_instructions = actual_instructions[0].to_le_bytes();

        tracing::debug!(
            "Semihosting check pc={pc:#x} instructions={0:#08x} {1:#08x} {2:#08x}",
            actual_instructions[0],
            actual_instructions[1],
            actual_instructions[2],
        );

        let mut expected_instruction = vec![];
        Instruction::Break(1, 14).encode_into_vec(&mut expected_instruction);
        let expected_instruction: [u8; 3] = [
            expected_instruction[0],
            expected_instruction[1],
            expected_instruction[2],
        ];

        tracing::debug!(
            "Expected instructions={0:#08x} {1:#08x} {2:#08x}",
            expected_instruction[0],
            expected_instruction[1],
            expected_instruction[2]
        );

        if &actual_instructions[..3] == expected_instruction.as_slice() {
            let a2: u32 = core.read_core_reg(RegisterId::from(2))?.try_into()?;
            let a3: u32 = core.read_core_reg(RegisterId::from(3))?.try_into()?;

            tracing::info!("Semihosting found pc={pc:#x} a2={a2:#x} a3={a3:#x}");

            reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(
                decode_semihosting_syscall(core, a2, a3)?,
            ));
        }
        Ok(reason)
    }
}

impl<'probe> MemoryInterface for Xtensa<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.interface.supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.interface.read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.interface.read_word_32(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.interface.read_word_16(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.interface.read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.interface.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.interface.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.interface.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.interface.read_8(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.interface.write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.interface.write_word_32(address, data)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.interface.write_word_16(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.interface.write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.interface.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.interface.write_32(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.interface.write_16(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write_8(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.interface.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.interface.flush()
    }
}

impl<'probe> CoreInterface for Xtensa<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.interface.wait_for_core_halted(timeout)?;
        self.state.pc_written = false;

        let status = self.status()?;

        tracing::debug!("Core halted: {:#?}", status);

        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        Ok(self.interface.is_halted()?)
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let status = if self.core_halted()? {
            let debug_cause = self.interface.read_register::<DebugCause>()?;
            let reason = if debug_cause.halt_reason()
                == HaltReason::Breakpoint(BreakpointCause::Software)
                && (debug_cause.break_instruction() || debug_cause.break_n_instruction())
            {
                // The chip initiated this halt, therefore we need to update pc_written state
                self.state.pc_written = false;
                // Check if the breakpoint is a semihosting call
                Xtensa::check_for_semihosting(debug_cause.halt_reason(), self)?
            } else {
                debug_cause.halt_reason()
            };
            CoreStatus::Halted(reason)
        } else {
            CoreStatus::Running
        };

        Ok(status)
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.halt()?;
        self.interface.wait_for_core_halted(timeout)?;

        self.core_info()
    }

    fn run(&mut self) -> Result<(), Error> {
        self.skip_breakpoint_instruction()?;
        Ok(self.interface.resume()?)
    }

    fn reset(&mut self) -> Result<(), Error> {
        Ok(self.interface.reset()?)
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.reset_and_halt(timeout)?;

        self.core_info()
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.skip_breakpoint_instruction()?;
        self.interface.step()?;
        self.state.pc_written = false;

        self.core_info()
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let register = Register::try_from(address)?;
        let value = self.interface.read_register_untyped(register)?;

        Ok(RegisterValue::U32(value))
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        let value: u32 = value.try_into()?;

        if address == self.program_counter().id {
            self.state.pc_written = true;
        }

        let register = Register::try_from(address)?;
        self.interface.write_register_untyped(register, value)?;

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(self.interface.available_breakpoint_units())
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = Vec::with_capacity(self.available_breakpoint_units()? as usize);

        let enabled_breakpoints = self.interface.read_register::<IBreakEn>()?;

        for i in 0..self.available_breakpoint_units()? as usize {
            let is_enabled = enabled_breakpoints.0 & (1 << i) != 0;
            let breakpoint = if is_enabled {
                let address = self
                    .interface
                    .read_register_untyped(Self::IBREAKA_REGS[i])?;

                Some(address as u64)
            } else {
                None
            };

            breakpoints.push(breakpoint);
        }

        Ok(breakpoints)
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.state.breakpoints_enabled = state;
        let mask = if state {
            self.state.breakpoint_mask()
        } else {
            0
        };

        self.interface.write_register(IBreakEn(mask))?;

        Ok(())
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.state.breakpoint_set[unit_index] = true;
        self.interface
            .write_register_untyped(Self::IBREAKA_REGS[unit_index], addr as u32)?;

        if self.state.breakpoints_enabled {
            let mask = self.state.breakpoint_mask();
            self.interface.write_register(IBreakEn(mask))?;
        }

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.state.breakpoint_set[unit_index] = false;

        if self.state.breakpoints_enabled {
            let mask = self.state.breakpoint_mask();
            self.interface.write_register(IBreakEn(mask))?;
        }

        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &XTENSA_CORE_REGSISTERS
    }

    fn program_counter(&self) -> &'static CoreRegister {
        &PC
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        &FP
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        &SP
    }

    fn return_address(&self) -> &'static CoreRegister {
        &RA
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Xtensa
    }

    fn core_type(&self) -> CoreType {
        CoreType::Xtensa
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        // TODO: NX exists, too
        Ok(InstructionSet::Xtensa)
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.interface.xdm.halt_on_reset(true);
        Ok(())
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.interface.xdm.halt_on_reset(false);
        Ok(())
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.interface.leave_ocd_mode()?;
        Ok(())
    }
}
