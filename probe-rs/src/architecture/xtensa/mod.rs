//! All the interface bits for Xtensa.

use std::{sync::Arc, time::Duration};

use probe_rs_target::{Architecture, CoreType, InstructionSet};
use zerocopy::IntoBytes;

use crate::{
    CoreInformation, CoreInterface, CoreRegister, CoreStatus, Error, HaltReason, MemoryInterface,
    architecture::xtensa::{
        arch::{
            CpuRegister, Register, SpecialRegister,
            instruction::{Instruction, InstructionEncoding},
        },
        communication_interface::{
            DebugCause, IBreakEn, ProgramStatus, WindowProperties, XtensaCommunicationInterface,
        },
        registers::{FP, PC, RA, SP, XTENSA_CORE_REGISTERS},
        sequences::XtensaDebugSequence,
        xdm::PowerStatus,
    },
    core::{
        BreakpointCause,
        registers::{CoreRegisters, RegisterId, RegisterValue},
    },
    semihosting::{SemihostingCommand, decode_semihosting_syscall},
};

pub(crate) mod arch;
pub(crate) mod xdm;

pub mod communication_interface;
pub(crate) mod register_cache;
pub mod registers;
pub(crate) mod sequences;

/// Xtensa core state.
#[derive(Debug)]
pub struct XtensaCoreState {
    /// Whether the core is enabled.
    enabled: bool,

    /// Whether hardware breakpoints are enabled.
    breakpoints_enabled: bool,

    /// Whether each hardware breakpoint is set.
    // 2 is the architectural upper limit. The actual count is stored in
    // [`communication_interface::XtensaInterfaceState`]
    breakpoint_set: [bool; 2],

    /// Whether the PC was written since we last halted. Used to avoid incrementing the PC on
    /// resume.
    pc_written: bool,

    /// The semihosting command that was decoded at the current program counter
    semihosting_command: Option<SemihostingCommand>,

    /// Whether the registers have been spilled to the stack.
    spilled: bool,
}

impl XtensaCoreState {
    /// Creates a new [`XtensaCoreState`].
    pub(crate) fn new() -> Self {
        Self {
            enabled: false,
            breakpoints_enabled: false,
            breakpoint_set: [false; 2],
            pc_written: false,
            semihosting_command: None,
            spilled: false,
        }
    }

    /// Creates a bitmask of the currently set breakpoints.
    fn breakpoint_mask(&self) -> u32 {
        self.breakpoint_set
            .iter()
            .enumerate()
            .fold(0, |acc, (i, &set)| if set { acc | (1 << i) } else { acc })
    }
}

/// An interface to operate an Xtensa core.
pub struct Xtensa<'probe> {
    interface: XtensaCommunicationInterface<'probe>,
    state: &'probe mut XtensaCoreState,
    sequence: Arc<dyn XtensaDebugSequence>,
}

impl<'probe> Xtensa<'probe> {
    const IBREAKA_REGS: [SpecialRegister; 2] =
        [SpecialRegister::IBreakA0, SpecialRegister::IBreakA1];

    /// Create a new Xtensa interface for a particular core.
    pub async fn new(
        interface: XtensaCommunicationInterface<'probe>,
        state: &'probe mut XtensaCoreState,
        sequence: Arc<dyn XtensaDebugSequence>,
    ) -> Result<Self, Error> {
        let mut this = Self {
            interface,
            state,
            sequence,
        };

        this.on_attach().await?;

        Ok(this)
    }

    fn clear_cache(&mut self) {
        self.state.spilled = false;
        self.interface.clear_register_cache();
    }

    async fn on_attach(&mut self) -> Result<(), Error> {
        // If the core was reset, force a reconnection.
        let core_reset;
        if self.state.enabled {
            let status = self
                .interface
                .xdm
                .power_status({
                    let mut clear_value = PowerStatus(0);
                    clear_value.set_core_was_reset(true);
                    clear_value.set_debug_was_reset(true);
                    clear_value
                })
                .await?;
            core_reset = status.core_was_reset() || !status.core_domain_on();
            let debug_reset = status.debug_was_reset() || !status.debug_domain_on();

            if core_reset {
                tracing::debug!("Core was reset");
                *self.state = XtensaCoreState::new();
            }
            if debug_reset {
                tracing::debug!("Debug was reset");
                self.state.enabled = false;
            }
        } else {
            core_reset = true;
        }

        // (Re)enter debug mode if necessary. This also checks if the core is enabled.
        if !self.state.enabled {
            // Enable debug module.
            self.interface.enter_debug_mode().await?;
            self.state.enabled = true;

            if core_reset {
                // Run the connection sequence while halted.
                let was_running = self
                    .interface
                    .halt_with_previous(Duration::from_millis(500))
                    .await?;

                self.sequence.on_connect(&mut self.interface).await?;

                if was_running {
                    self.run().await?;
                }
            }
        }

        Ok(())
    }

    async fn core_info(&mut self) -> Result<CoreInformation, Error> {
        let pc = self.read_core_reg(self.program_counter().id).await?;

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    async fn skip_breakpoint(&mut self) -> Result<(), Error> {
        self.state.semihosting_command = None;
        if !self.state.pc_written {
            let debug_cause = self.debug_cause().await?;

            let pc_increment = if debug_cause.break_instruction() {
                3
            } else if debug_cause.break_n_instruction() {
                2
            } else {
                0
            };

            if pc_increment > 0 {
                // Step through the breakpoint
                let mut pc_value = self
                    .interface
                    .read_register_untyped(Register::CurrentPc)
                    .await?;
                pc_value += pc_increment;
                self.interface
                    .write_register_untyped(Register::CurrentPc, pc_value)
                    .await?;
            } else if debug_cause.ibreak_exception() {
                let pc_value = self
                    .interface
                    .read_register_untyped(Register::CurrentPc)
                    .await?;
                let bps = self.hw_breakpoints().await?;
                if let Some(bp_unit) = bps.iter().position(|bp| *bp == Some(pc_value as u64)) {
                    // Disable the breakpoint
                    self.clear_hw_breakpoint(bp_unit).await?;
                    // Single step
                    let ps = self.current_ps().await?;
                    self.interface.step(1, ps.intlevel()).await?;
                    // Re-enable the breakpoint
                    self.set_hw_breakpoint(bp_unit, pc_value as u64).await?;
                }
            }
        }

        Ok(())
    }

    /// Check if the current breakpoint is a semihosting call
    // OpenOCD implementation: https://github.com/espressif/openocd-esp32/blob/93dd01511fd13d4a9fb322cd9b600c337becef9e/src/target/espressif/esp_xtensa_semihosting.c#L42-L103
    async fn check_for_semihosting(&mut self) -> Result<Option<SemihostingCommand>, Error> {
        const SEMI_BREAK: u32 = const {
            let InstructionEncoding::Narrow(bytes) = Instruction::Break(1, 14).encode();
            bytes
        };

        // We only want to decode the semihosting command once, since answering it might change some of the registers
        if let Some(command) = self.state.semihosting_command {
            return Ok(Some(command));
        }

        let pc: u64 = self
            .read_core_reg(self.program_counter().id)
            .await?
            .try_into()?;

        let mut actual_instruction = [0u8; 3];
        self.read_8(pc, &mut actual_instruction).await?;
        let actual_instruction = u32::from_le_bytes([
            actual_instruction[0],
            actual_instruction[1],
            actual_instruction[2],
            0,
        ]);

        tracing::debug!("Semihosting check pc={pc:#x} instruction={actual_instruction:#010x}");

        let command = if actual_instruction == SEMI_BREAK {
            let syscall = decode_semihosting_syscall(self).await?;
            if let SemihostingCommand::Unknown(details) = syscall {
                self.sequence
                    .clone()
                    .on_unknown_semihosting_command(self, details)
                    .await?
            } else {
                Some(syscall)
            }
        } else {
            None
        };
        self.state.semihosting_command = command;

        Ok(command)
    }

    async fn on_halted(&mut self) -> Result<(), Error> {
        self.state.pc_written = false;
        self.clear_cache();

        let status = self.status().await?;
        tracing::debug!("Core halted: {:#?}", status);

        if status.is_halted() {
            self.sequence.on_halt(&mut self.interface).await?;
        }

        Ok(())
    }

    async fn halt_with_previous(&mut self, timeout: Duration) -> Result<bool, Error> {
        let was_running = self.interface.halt_with_previous(timeout).await?;
        if was_running {
            self.on_halted().await?;
        }

        Ok(was_running)
    }

    async fn halted_access<F, T>(&mut self, op: F) -> Result<T, Error>
    where
        F: AsyncFnOnce(&mut Self) -> Result<T, Error>,
    {
        let was_running = self.halt_with_previous(Duration::from_millis(100)).await?;

        let result = op(self).await;

        if was_running {
            self.run().await?;
        }

        result
    }

    async fn current_ps(&mut self) -> Result<ProgramStatus, Error> {
        // Reading ProgramStatus using `read_register` would return the value
        // after the debug interrupt has been taken.
        Ok(self
            .interface
            .read_register_untyped(Register::CurrentPs)
            .await
            .map(ProgramStatus)?)
    }

    async fn debug_cause(&mut self) -> Result<DebugCause, Error> {
        Ok(self.interface.read_register::<DebugCause>().await?)
    }

    async fn spill_registers(&mut self) -> Result<(), Error> {
        if self.state.spilled {
            return Ok(());
        }
        self.state.spilled = true;

        let register_file = RegisterFile::read(
            self.interface.core_properties().window_option_properties,
            &mut self.interface,
        )
        .await?;

        let window_reg_count = register_file.core.window_regs;
        for reg in 0..window_reg_count {
            let reg =
                CpuRegister::try_from(reg).expect("Could not convert register to CpuRegister");
            let value = register_file.read_register(reg);
            self.interface
                .state
                .register_cache
                .store(Register::Cpu(reg), value);
        }

        if self.current_ps().await?.excm() {
            // We are in an exception, possibly WindowOverflowN or WindowUnderflowN.
            // We can't spill registers in this state.
            return Ok(());
        }
        if self.current_ps().await?.woe() {
            // We should only spill registers if PS.WOE is set. According to the debug guide, we
            // also should not spill if INTLEVEL != 0 but I don't see why.
            register_file.spill(&mut self.interface).await?;
        }

        Ok(())
    }
}

// We can't use CoreMemoryInterface here, because we need to spill registers before reading.
// This needs to be considerably cleaned up.
#[async_trait::async_trait(?Send)]
impl MemoryInterface for Xtensa<'_> {
    async fn supports_native_64bit_access(&mut self) -> bool {
        self.interface.supports_native_64bit_access().await
    }

    async fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.halted_access(async |this| {
            this.spill_registers().await?;

            this.interface.read_word_64(address).await
        })
        .await
    }

    async fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.halted_access(async |this| {
            this.spill_registers().await?;

            this.interface.read_word_32(address).await
        })
        .await
    }

    async fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.halted_access(async |this| {
            this.spill_registers().await?;

            this.interface.read_word_16(address).await
        })
        .await
    }

    async fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.halted_access(async |this| {
            this.spill_registers().await?;

            this.interface.read_word_8(address).await
        })
        .await
    }

    async fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.read_8(address, data.as_mut_bytes()).await
    }

    async fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.read_8(address, data.as_mut_bytes()).await
    }

    async fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.read_8(address, data.as_mut_bytes()).await
    }

    async fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.halted_access(async |this| {
            this.spill_registers().await?;

            this.interface.read_8(address, data).await
        })
        .await
    }

    async fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.interface.write_word_64(address, data).await
    }

    async fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.interface.write_word_32(address, data).await
    }

    async fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.interface.write_word_16(address, data).await
    }

    async fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.interface.write_word_8(address, data).await
    }

    async fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.interface.write_64(address, data).await
    }

    async fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.interface.write_32(address, data).await
    }

    async fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.interface.write_16(address, data).await
    }

    async fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write_8(address, data).await
    }

    async fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.write(address, data).await
    }

    async fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.interface.supports_8bit_transfers().await
    }

    async fn flush(&mut self) -> Result<(), Error> {
        self.interface.flush().await
    }
}

#[async_trait::async_trait(?Send)]
impl CoreInterface for Xtensa<'_> {
    async fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.interface.wait_for_core_halted(timeout).await?;
        self.on_halted().await?;

        Ok(())
    }

    async fn core_halted(&mut self) -> Result<bool, Error> {
        let was_halted = self.interface.state.is_halted;
        let is_halted = self.interface.core_halted().await?;

        if !was_halted && is_halted {
            self.on_halted().await?;
        }

        Ok(is_halted)
    }

    async fn status(&mut self) -> Result<CoreStatus, Error> {
        let status = if self.core_halted().await? {
            let debug_cause = self.debug_cause().await?;

            let mut reason = debug_cause.halt_reason();
            if reason == HaltReason::Breakpoint(BreakpointCause::Software) {
                // The chip initiated this halt, therefore we need to update pc_written state
                self.state.pc_written = false;
                // Check if the breakpoint is a semihosting call
                if let Some(cmd) = self.check_for_semihosting().await? {
                    reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd));
                }
            }

            CoreStatus::Halted(reason)
        } else {
            CoreStatus::Running
        };

        Ok(status)
    }

    async fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.halt_with_previous(timeout).await?;

        self.core_info().await
    }

    async fn run(&mut self) -> Result<(), Error> {
        self.skip_breakpoint().await?;
        Ok(self.interface.resume_core().await?)
    }

    async fn reset(&mut self) -> Result<(), Error> {
        self.reset_and_halt(Duration::from_millis(500)).await?;

        self.run().await
    }

    async fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.state.semihosting_command = None;
        self.sequence
            .reset_system_and_halt(&mut self.interface, timeout)
            .await?;
        self.on_halted().await?;

        // TODO: this may return that the core has gone away, which is fine but currently unexpected
        self.on_attach().await?;

        self.core_info().await
    }

    async fn step(&mut self) -> Result<CoreInformation, Error> {
        self.skip_breakpoint().await?;

        // Only count instructions in the current context.
        let ps = self.current_ps().await?;
        self.interface.step(1, ps.intlevel()).await?;

        self.on_halted().await?;

        self.core_info().await
    }

    async fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        self.halted_access(async |this| {
            let register = Register::try_from(address)?;
            let value = this.interface.read_register_untyped(register).await?;

            Ok(RegisterValue::U32(value))
        })
        .await
    }

    async fn write_core_reg(
        &mut self,
        address: RegisterId,
        value: RegisterValue,
    ) -> Result<(), Error> {
        self.halted_access(async |this| {
            let value: u32 = value.try_into()?;

            if address == this.program_counter().id {
                this.state.pc_written = true;
            }

            let register = Register::try_from(address)?;
            this.interface
                .write_register_untyped(register, value)
                .await?;

            Ok(())
        })
        .await
    }

    async fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        Ok(self.interface.available_breakpoint_units())
    }

    async fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        self.halted_access(async |this| {
            let mut breakpoints =
                Vec::with_capacity(this.available_breakpoint_units().await? as usize);

            let enabled_breakpoints = this.interface.read_register::<IBreakEn>().await?;

            for i in 0..this.available_breakpoint_units().await? as usize {
                let is_enabled = enabled_breakpoints.0 & (1 << i) != 0;
                let breakpoint = if is_enabled {
                    let address = this
                        .interface
                        .read_register_untyped(Self::IBREAKA_REGS[i])
                        .await?;

                    Some(address as u64)
                } else {
                    None
                };

                breakpoints.push(breakpoint);
            }

            Ok(breakpoints)
        })
        .await
    }

    async fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.halted_access(async |this| {
            this.state.breakpoints_enabled = state;
            let mask = if state {
                this.state.breakpoint_mask()
            } else {
                0
            };

            this.interface.write_register(IBreakEn(mask)).await?;

            Ok(())
        })
        .await
    }

    async fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.halted_access(async |this| {
            this.state.breakpoint_set[unit_index] = true;
            this.interface
                .write_register_untyped(Self::IBREAKA_REGS[unit_index], addr as u32)
                .await?;

            if this.state.breakpoints_enabled {
                let mask = this.state.breakpoint_mask();
                this.interface.write_register(IBreakEn(mask)).await?;
            }

            Ok(())
        })
        .await
    }

    async fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.halted_access(async |this| {
            this.state.breakpoint_set[unit_index] = false;

            if this.state.breakpoints_enabled {
                let mask = this.state.breakpoint_mask();
                this.interface.write_register(IBreakEn(mask)).await?;
            }

            Ok(())
        })
        .await
    }

    fn registers(&self) -> &'static CoreRegisters {
        &XTENSA_CORE_REGISTERS
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

    async fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        // TODO: NX exists, too
        Ok(InstructionSet::Xtensa)
    }

    async fn fpu_support(&mut self) -> Result<bool, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(false)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        // TODO: ESP32 and ESP32-S3 have FPU
        Ok(0)
    }

    async fn reset_catch_set(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("reset_catch_set"))
    }

    async fn reset_catch_clear(&mut self) -> Result<(), Error> {
        Err(Error::NotImplemented("reset_catch_clear"))
    }

    async fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.interface.leave_debug_mode().await?;
        Ok(())
    }
}

struct RegisterFile {
    core: WindowProperties,
    registers: Vec<u32>,
    window_base: u8,
    window_start: u32,
}

impl RegisterFile {
    async fn read(
        xtensa: WindowProperties,
        interface: &mut XtensaCommunicationInterface<'_>,
    ) -> Result<Self, Error> {
        let window_base_result = interface
            .schedule_read_register(SpecialRegister::Windowbase)
            .await?;
        let window_start_result = interface
            .schedule_read_register(SpecialRegister::Windowstart)
            .await?;

        let mut register_results = Vec::with_capacity(xtensa.num_aregs as usize);

        let ar0 = arch::CpuRegister::A0 as u8;

        // Restore registers before reading them, as reading a special
        // register overwrote scratch registers.
        interface.restore_registers().await?;
        // The window registers alias each other, so we need to make sure we don't read
        // the cached values. We'll then use the registers we read here to prime the cache.
        for ar in ar0..ar0 + xtensa.window_regs {
            let reg = CpuRegister::try_from(ar)?;
            interface.state.register_cache.remove(reg.into());
        }

        for _ in 0..xtensa.num_aregs / xtensa.window_regs {
            // Read registers visible in the current window
            for ar in ar0..ar0 + xtensa.window_regs {
                let reg = CpuRegister::try_from(ar)?;
                let result = interface.schedule_read_register(reg).await?;

                register_results.push(result);
            }

            // Rotate window to see the next `window_regs` registers
            let rotw_arg = xtensa.window_regs / xtensa.rotw_rotates;
            interface
                .xdm
                .schedule_execute_instruction(Instruction::Rotw(rotw_arg))
                .await;
        }

        // Now do the actual read.
        interface
            .xdm
            .execute()
            .await
            .expect("Failed to execute read. This shouldn't happen.");

        let mut register_values = vec![];
        for result in register_results.into_iter() {
            if let Ok(value) = interface.read_deferred_result(result).await {
                register_values.push(value);
            }
        }

        // WindowBase points to the first register of the current window in the register file.
        // In essence, it selects which 16 registers are visible out of the 64 physical registers.
        let window_base = interface.read_deferred_result(window_base_result).await?;

        // The WindowStart Special Register, which is also added by the option and consists of
        // NAREG/4 bits. Each call frame, which has not been spilled, is represented by a bit in the
        // WindowStart register. The call frame's bit is set in the position given by the current
        // WindowBase register value.
        // Each register window has a single bit in the WindowStart register. The window size
        // can be calculated by the difference of bit positions in the WindowStart register.
        // For example, if WindowBase is 6, and WindowStart is 0b0000000001011001:
        // - The first window uses a0-a11, and executed a CALL12 or CALLX12 instruction.
        // - The second window uses a0-a3, and executed a CALL14 instruction.
        // - The third window uses a0-a7, and executed a CALL8 instruction.
        // - The fourth window is free to use all 16 registers at this point.
        // There are no more active windows.
        // This value is used by the hardware to determine if WindowOverflow or WindowUnderflow
        // exceptions should be raised. The exception handlers then spill or reload registers
        // from the stack and set/clear the corresponding bit in the WindowStart register.
        let window_start = interface.read_deferred_result(window_start_result).await?;

        // We have read registers relative to the current windowbase. Let's
        // rotate the registers back so that AR0 is at index 0.
        register_values.rotate_right((window_base * xtensa.rotw_rotates as u32) as usize);

        Ok(Self {
            core: xtensa,
            registers: register_values,
            window_base: window_base as u8,
            window_start,
        })
    }

    async fn spill(&self, xtensa: &mut XtensaCommunicationInterface<'_>) -> Result<(), Error> {
        if !self.core.has_windowed_registers {
            return Ok(());
        }

        if self.window_start == 0 {
            // There are no stack frames to spill.
            return Ok(());
        }

        // Quoting the debug guide:
        // The proper thing for the debugger to do when doing a call traceback or whenever looking
        // at a calling function's address registers, is to examine the WINDOWSTART register bits
        // (in combination with WINDOWBASE) to determine whether to look in the register file or on
        // the stack for the relevant address register values.
        // Quote ends here
        // The above describes what we should do for minimally intrusive debugging, but I don't
        // think we have the option to do this. Instead we spill everything that needs to be
        // spilled, and then we can use the stack to unwind the registers. While forcefully
        // spilling is not faithful to the true code execution, it is relatively simple to do.
        // The debug guide states this can be noticeably slow, for example if "step to next line" is
        // implemented by stepping one instruction at a time until the line number changes.
        // FIXME: we can improve the above issue by only spilling before reading memory.

        // Process registers. We need to roll the windows back by the window increment, and copy
        // register values to the stack, if the relevant WindowStart bit is set. The window
        // increment of the current window is saved in the top 2 bits of A0 (the return address).

        // Find oldest window.
        let mut window_base = self.next_window_base(self.window_base);

        while let Some(window) = RegisterWindow::at_windowbase(self, window_base) {
            window.spill(xtensa).await?;
            window_base = self.next_window_base(window_base);

            if window_base == self.window_base {
                // We are back to the original window, so we can stop.

                // We are not spilling the first window. We don't have a destination for it, and we don't
                // need to unwind it from the stack.
                break;
            }
        }

        Ok(())
    }

    fn is_window_start(&self, windowbase: u8) -> bool {
        self.window_start & 1 << (windowbase % self.core.windowbase_size()) != 0
    }

    fn next_window_base(&self, window_base: u8) -> u8 {
        let mut wb = (window_base + 1) % self.core.windowbase_size();
        while wb != window_base {
            if self.is_window_start(wb) {
                break;
            }
            wb = (wb + 1) % self.core.windowbase_size();
        }

        wb
    }

    fn wb_offset_to_canonical(&self, idx: u8) -> u8 {
        (idx + self.window_base * self.core.rotw_rotates) % self.core.num_aregs
    }

    fn read_register(&self, reg: CpuRegister) -> u32 {
        let index = self.wb_offset_to_canonical(reg as u8);
        self.registers[index as usize]
    }
}

struct RegisterWindow<'a> {
    window_base: u8,

    /// In units of window_base bits.
    window_size: u8,

    file: &'a RegisterFile,
}

impl<'a> RegisterWindow<'a> {
    fn at_windowbase(file: &'a RegisterFile, window_base: u8) -> Option<Self> {
        if !file.is_window_start(window_base) {
            return None;
        }

        let next_window_base = file.next_window_base(window_base);
        let window_size = (next_window_base + file.core.windowbase_size() - window_base)
            % file.core.windowbase_size();

        Some(Self {
            window_base,
            file,
            window_size: window_size.min(3),
        })
    }

    /// Register spilling needs access to other frames' stack pointers.
    fn read_register(&self, reg: CpuRegister) -> u32 {
        let index = self.wb_offset_to_canonical(reg as u8);
        self.file.registers[index as usize]
    }

    fn wb_offset_to_canonical(&self, idx: u8) -> u8 {
        (idx + self.window_base * self.file.core.rotw_rotates) % self.file.core.num_aregs
    }

    async fn spill(&self, interface: &mut XtensaCommunicationInterface<'_>) -> Result<(), Error> {
        // a0-a3 goes into our stack, the rest into the stack of the caller.
        let a0_a3 = [
            self.read_register(CpuRegister::A0),
            self.read_register(CpuRegister::A1),
            self.read_register(CpuRegister::A2),
            self.read_register(CpuRegister::A3),
        ];

        match self.window_size {
            0 => {} // Nowhere to spill to

            1 => {
                interface
                    .write_32(self.read_register(CpuRegister::A5) as u64 - 16, &a0_a3)
                    .await?
            }

            // Spill a4-a7
            2 => {
                let sp = interface
                    .read_word_32(self.read_register(CpuRegister::A1) as u64 - 12)
                    .await?;

                interface
                    .write_32(self.read_register(CpuRegister::A9) as u64 - 16, &a0_a3)
                    .await?;

                // Enable check at INFO level to avoid spamming the logs.
                if tracing::enabled!(tracing::Level::INFO) {
                    // In some cases (spilling on each halt),
                    // this readback comes back as 0 for some reason. This assertion is temporarily
                    // meant to help me debug this.
                    let written = interface
                        .read_word_32(self.read_register(CpuRegister::A9) as u64 - 12)
                        .await?;
                    assert!(
                        written == self.read_register(CpuRegister::A1),
                        "Failed to spill A1. Expected {:#x}, got {:#x}",
                        self.read_register(CpuRegister::A1),
                        written
                    );
                }

                let regs = [
                    self.read_register(CpuRegister::A4),
                    self.read_register(CpuRegister::A5),
                    self.read_register(CpuRegister::A6),
                    self.read_register(CpuRegister::A7),
                ];
                interface.write_32(sp as u64 - 32, &regs).await?;
            }

            // Spill a4-a11
            3 => {
                let sp = interface
                    .read_word_32(self.read_register(CpuRegister::A1) as u64 - 12)
                    .await?;
                interface
                    .write_32(self.read_register(CpuRegister::A13) as u64 - 16, &a0_a3)
                    .await?;

                let regs = [
                    self.read_register(CpuRegister::A4),
                    self.read_register(CpuRegister::A5),
                    self.read_register(CpuRegister::A6),
                    self.read_register(CpuRegister::A7),
                    self.read_register(CpuRegister::A8),
                    self.read_register(CpuRegister::A9),
                    self.read_register(CpuRegister::A10),
                    self.read_register(CpuRegister::A11),
                ];
                interface.write_32(sp as u64 - 48, &regs).await?;
            }

            // There is no such thing as spilling a12-a15 - there can be only 12 active registers in
            // a stack frame that is not the topmost, as there is no CALL16 instruction.
            _ => unreachable!(),
        }

        Ok(())
    }
}
