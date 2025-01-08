//! All the interface bits for RISC-V.

use crate::{
    architecture::riscv::sequences::RiscvDebugSequence,
    core::{
        Architecture, BreakpointCause, CoreInformation, CoreRegisters, RegisterId, RegisterValue,
    },
    memory::{valid_32bit_address, CoreMemoryInterface},
    memory_mapped_bitfield_register,
    probe::DebugProbeError,
    semihosting::decode_semihosting_syscall,
    semihosting::SemihostingCommand,
    CoreInterface, CoreRegister, CoreStatus, CoreType, Error, HaltReason, InstructionSet,
    MemoryInterface, MemoryMappedRegister,
};
use bitfield::bitfield;
use communication_interface::{AbstractCommandErrorKind, RiscvCommunicationInterface, RiscvError};
use registers::{FP, RA, RISCV_CORE_REGISTERS, SP};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

#[macro_use]
pub mod registers;
pub use registers::PC;
pub(crate) mod assembly;
pub mod communication_interface;
pub(crate) mod dtm;
pub mod sequences;

/// An interface to operate a RISC-V core.
pub struct Riscv32<'state> {
    interface: RiscvCommunicationInterface<'state>,
    state: &'state mut RiscvCoreState,
    sequence: Arc<dyn RiscvDebugSequence>,
}

impl<'state> Riscv32<'state> {
    /// Create a new RISC-V interface for a particular hart.
    pub fn new(
        interface: RiscvCommunicationInterface<'state>,
        state: &'state mut RiscvCoreState,
        sequence: Arc<dyn RiscvDebugSequence>,
    ) -> Result<Self, RiscvError> {
        Ok(Self {
            interface,
            state,
            sequence,
        })
    }

    fn read_csr(&mut self, address: u16) -> Result<u32, RiscvError> {
        self.interface.read_csr(address)
    }

    fn write_csr(&mut self, address: u16, value: u32) -> Result<(), RiscvError> {
        tracing::debug!("Writing CSR {:#x}", address);

        match self.interface.abstract_cmd_register_write(address, value) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                tracing::debug!("Could not write core register {:#x} with abstract command, falling back to program buffer", address);
                self.interface.write_csr_progbuf(address, value)
            }
            other => other,
        }
    }

    /// Resume the core.
    fn resume_core(&mut self) -> Result<(), crate::Error> {
        self.state.semihosting_command = None;
        self.interface.resume_core()?;

        Ok(())
    }

    /// Check if the current breakpoint is a semihosting call
    fn check_for_semihosting(&mut self) -> Result<Option<SemihostingCommand>, Error> {
        // The Riscv Semihosting Specification, specificies the following sequence of instructions,
        // to trigger a semihosting call:
        // <https://github.com/riscv-software-src/riscv-semihosting/blob/main/riscv-semihosting-spec.adoc>

        const TRAP_INSTRUCTIONS: [u32; 3] = [
            0x01f01013, // slli x0, x0, 0x1f (Entry Nop)
            0x00100073, // ebreak (Break to debugger)
            0x40705013, // srai x0, x0, 7 (NOP encoding the semihosting call number 7)
        ];

        // We only want to decode the semihosting command once, since answering it might change some of the registers
        if let Some(command) = self.state.semihosting_command {
            return Ok(Some(command));
        }

        let pc: u32 = self.read_core_reg(self.program_counter().id)?.try_into()?;

        // Read the actual instructions, starting at the instruction before the ebreak (PC-4)
        let mut actual_instructions = [0u32; 3];
        self.read_32((pc - 4) as u64, &mut actual_instructions)?;
        let actual_instructions = actual_instructions.as_slice();

        tracing::debug!(
            "Semihosting check pc={pc:#x} instructions={0:#08x} {1:#08x} {2:#08x}",
            actual_instructions[0],
            actual_instructions[1],
            actual_instructions[2]
        );

        let command = if TRAP_INSTRUCTIONS == actual_instructions {
            Some(decode_semihosting_syscall(self)?)
        } else {
            None
        };
        self.state.semihosting_command = command;

        Ok(command)
    }

    fn determine_number_of_hardware_breakpoints(&mut self) -> Result<u32, RiscvError> {
        tracing::debug!("Determining number of HW breakpoints supported");

        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tinfo = 0x7a4;

        let mut tselect_index = 0;

        // These steps follow the debug specification 0.13, section 5.1 Enumeration
        loop {
            tracing::debug!("Trying tselect={}", tselect_index);
            if let Err(e) = self.write_csr(tselect, tselect_index) {
                match e {
                    RiscvError::AbstractCommand(AbstractCommandErrorKind::Exception) => break,
                    other_error => return Err(other_error),
                }
            }

            let readback = self.read_csr(tselect)?;

            if readback != tselect_index {
                break;
            }

            match self.read_csr(tinfo) {
                Ok(tinfo_val) => {
                    if tinfo_val & 0xffff == 1 {
                        // Trigger doesn't exist, break the loop
                        break;
                    } else {
                        tracing::info!(
                            "Discovered trigger with index {} and type {}",
                            tselect_index,
                            tinfo_val & 0xffff
                        );
                    }
                }
                Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::Exception)) => {
                    // An exception means we have to read tdata1 to discover the type
                    let tdata_val = self.read_csr(tdata1)?;

                    // Read the mxl field from the misa register (see RISC-V Privileged Spec, 3.1.1)
                    let misa_value = Misa(self.read_csr(0x301)?);
                    let xlen = u32::pow(2, misa_value.mxl() + 4);

                    let trigger_type = tdata_val >> (xlen - 4);

                    if trigger_type == 0 {
                        break;
                    }

                    tracing::info!(
                        "Discovered trigger with index {} and type {}",
                        tselect_index,
                        trigger_type,
                    );
                }
                Err(other) => return Err(other),
            }

            tselect_index += 1;
        }

        tracing::debug!("Target supports {} breakpoints.", tselect_index);

        Ok(tselect_index)
    }
}

impl CoreInterface for Riscv32<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), crate::Error> {
        self.interface.wait_for_core_halted(timeout)?;
        self.state.pc_written = false;
        Ok(())
    }

    fn core_halted(&mut self) -> Result<bool, crate::Error> {
        Ok(self.interface.core_halted()?)
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, crate::Error> {
        // TODO: We should use hartsum to determine if any hart is halted
        //       quickly

        let status: Dmstatus = self.interface.read_dm_register()?;

        if status.allhalted() {
            // determine reason for halt
            let dcsr = Dcsr(self.read_core_reg(RegisterId::from(0x7b0))?.try_into()?);

            let reason = match dcsr.cause() {
                // An ebreak instruction was hit
                1 => {
                    // The chip initiated this halt, therefore we need to update pc_written state
                    self.state.pc_written = false;
                    if let Some(cmd) = self.check_for_semihosting()? {
                        HaltReason::Breakpoint(BreakpointCause::Semihosting(cmd))
                    } else {
                        HaltReason::Breakpoint(BreakpointCause::Software)
                    }
                    // TODO: Add testcase to probe-rs-debugger-test to validate semihosting exit/abort work and unknown semihosting operations are skipped
                }
                // Trigger module caused halt
                2 => HaltReason::Breakpoint(BreakpointCause::Hardware),
                // Debugger requested a halt
                3 => HaltReason::Request,
                // Core halted after single step
                4 => HaltReason::Step,
                // Core halted directly after reset
                5 => HaltReason::Exception,
                // Reserved for future use in specification
                _ => HaltReason::Unknown,
            };

            Ok(CoreStatus::Halted(reason))
        } else if status.allrunning() {
            Ok(CoreStatus::Running)
        } else {
            Err(Error::Other(
                "Some cores are running while some are halted, this should not happen.".to_string(),
            ))
        }
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.interface.halt(timeout)?;
        Ok(self.interface.core_info()?)
    }

    fn run(&mut self) -> Result<(), Error> {
        if !self.state.pc_written {
            // Before we run, we always perform a single instruction step, to account for possible breakpoints that might get us stuck on the current instruction.
            self.step()?;
        }

        // resume the core.
        self.resume_core()?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.reset_and_halt(Duration::from_secs(1))?;
        self.resume_core()?;

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence
            .reset_system_and_halt(&mut self.interface, timeout)?;

        let pc = self.read_core_reg(RegisterId(0x7b1))?;

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn step(&mut self) -> Result<CoreInformation, crate::Error> {
        let halt_reason = self.status()?;
        if matches!(
            halt_reason,
            CoreStatus::Halted(HaltReason::Breakpoint(
                BreakpointCause::Software | BreakpointCause::Semihosting(_)
            ))
        ) {
            // If we are halted on a software breakpoint, we can skip the single step and manually advance the dpc.
            let mut debug_pc = self.read_core_reg(RegisterId(0x7b1))?;
            // Advance the dpc by the size of the EBREAK (ebreak or c.ebreak) instruction.
            if matches!(self.instruction_set()?, InstructionSet::RV32C) {
                // We may have been halted by either an EBREAK or a C.EBREAK instruction.
                // We need to read back the instruction to determine how many bytes we need to skip.
                let instruction = self.read_word_32(debug_pc.try_into().unwrap())?;
                if instruction & 0x3 != 0x3 {
                    // Compressed instruction.
                    debug_pc.increment_address(2)?;
                } else {
                    debug_pc.increment_address(4)?;
                }
            } else {
                debug_pc.increment_address(4)?;
            }

            self.write_core_reg(RegisterId(0x7b1), debug_pc)?;
            return Ok(CoreInformation {
                pc: debug_pc.try_into()?,
            });
        } else if matches!(
            halt_reason,
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
        ) {
            // If we are halted on a hardware breakpoint.
            self.enable_breakpoints(false)?;
        }

        let mut dcsr = Dcsr(self.read_core_reg(RegisterId(0x7b0))?.try_into()?);
        // Set it up, so that the next `self.run()` will only do a single step
        dcsr.set_step(true);
        // Disable any interrupts during single step.
        dcsr.set_stepie(false);
        dcsr.set_stopcount(true);
        self.write_csr(0x7b0, dcsr.0)?;

        // Now we can resume the core for the single step.
        self.resume_core()?;
        self.wait_for_core_halted(Duration::from_millis(100))?;

        let pc = self.read_core_reg(RegisterId(0x7b1))?;

        // clear step request
        let mut dcsr = Dcsr(self.read_core_reg(RegisterId(0x7b0))?.try_into()?);
        dcsr.set_step(false);
        //Re-enable interrupts for single step.
        dcsr.set_stepie(true);
        dcsr.set_stopcount(false);
        self.write_csr(0x7b0, dcsr.0)?;

        // Re-enable breakpoints before we continue.
        if matches!(
            halt_reason,
            CoreStatus::Halted(HaltReason::Breakpoint(BreakpointCause::Hardware))
        ) {
            // If we are halted on a hardware breakpoint.
            self.enable_breakpoints(true)?;
        }

        self.state.pc_written = false;
        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, crate::Error> {
        self.read_csr(address.0)
            .map(|v| v.into())
            .map_err(|e| e.into())
    }

    fn write_core_reg(
        &mut self,
        address: RegisterId,
        value: RegisterValue,
    ) -> Result<(), crate::Error> {
        let value: u32 = value.try_into()?;

        if address == self.program_counter().id {
            self.state.pc_written = true;
        }

        self.write_csr(address.0, value).map_err(|e| e.into())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, crate::Error> {
        match self.state.hw_breakpoints {
            Some(bp) => Ok(bp),
            None => {
                let bp = self.determine_number_of_hardware_breakpoints()?;
                self.state.hw_breakpoints = Some(bp);
                Ok(bp)
            }
        }
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait
    /// NOTE: For riscv, this assumes that only execution breakpoints are used.
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        // this can be called w/o halting the core via Session::new - temporarily halt if not halted

        let was_running = !self.core_halted()?;
        if was_running {
            self.halt(Duration::from_millis(100))?;
        }

        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;
        for bp_unit_index in 0..num_hw_breakpoints {
            // Select the trigger.
            self.write_csr(tselect, bp_unit_index as u32)?;

            // Read the trigger "configuration" data.
            let tdata_value = Mcontrol(self.read_csr(tdata1)?);

            tracing::debug!("Breakpoint {}: {:?}", bp_unit_index, tdata_value);

            // The trigger must be active in at least a single mode
            let trigger_any_mode_active = tdata_value.m() || tdata_value.s() || tdata_value.u();

            let trigger_any_action_enabled =
                tdata_value.execute() || tdata_value.store() || tdata_value.load();

            // Only return if the trigger if it is for an execution debug action in all modes.
            if tdata_value.type_() == 0b10
                && tdata_value.action() == 1
                && tdata_value.match_() == 0
                && trigger_any_mode_active
                && trigger_any_action_enabled
            {
                let breakpoint = self.read_csr(tdata2)?;
                breakpoints.push(Some(breakpoint as u64));
            } else {
                breakpoints.push(None);
            }
        }

        if was_running {
            self.resume_core()?;
        }

        Ok(breakpoints)
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), crate::Error> {
        // Loop through all triggers, and enable/disable them.
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;

        for bp_unit_index in 0..self.available_breakpoint_units()? as usize {
            // Select the trigger.
            self.write_csr(tselect, bp_unit_index as u32)?;

            // Read the trigger "configuration" data.
            let mut tdata_value = Mcontrol(self.read_csr(tdata1)?);

            // Only modify the trigger if it is for an execution debug action in all modes(probe-rs enabled it) or no modes (we previously disabled it).
            if tdata_value.type_() == 2
                && tdata_value.action() == 1
                && tdata_value.match_() == 0
                && tdata_value.execute()
                && ((tdata_value.m() && tdata_value.u()) || (!tdata_value.m() && !tdata_value.u()))
            {
                tracing::debug!(
                    "Will modify breakpoint enabled={} for {}: {:?}",
                    state,
                    bp_unit_index,
                    tdata_value
                );
                tdata_value.set_m(state);
                tdata_value.set_u(state);
                self.write_csr(tdata1, tdata_value.0)?;
            }
        }

        self.state.hw_breakpoints_enabled = state;
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u64) -> Result<(), crate::Error> {
        let addr = valid_32bit_address(addr)?;

        // select requested trigger
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        tracing::info!("Setting breakpoint {}", bp_unit_index);

        self.write_csr(tselect, bp_unit_index as u32)?;

        // verify the trigger has the correct type

        let tdata_value = Mcontrol(self.read_csr(tdata1)?);

        // This should not happen
        let trigger_type = tdata_value.type_();
        if trigger_type != 0b10 {
            return Err(RiscvError::UnexpectedTriggerType(trigger_type).into());
        }

        // Setup the trigger

        let mut instruction_breakpoint = Mcontrol(0);

        // Enter debug mode
        instruction_breakpoint.set_action(1);

        // Match exactly the value in tdata2
        instruction_breakpoint.set_type(2);
        instruction_breakpoint.set_match(0);

        instruction_breakpoint.set_m(true);

        instruction_breakpoint.set_u(true);

        // Trigger when instruction is executed
        instruction_breakpoint.set_execute(true);

        instruction_breakpoint.set_dmode(true);

        // Match address
        instruction_breakpoint.set_select(false);

        self.write_csr(tdata1, 0)?;
        self.write_csr(tdata2, addr)?;
        self.write_csr(tdata1, instruction_breakpoint.0)?;

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), crate::Error> {
        // this can be called w/o halting the core via Session::new - temporarily halt if not halted
        tracing::info!("Clearing breakpoint {}", unit_index);

        let was_running = !self.core_halted()?;
        if was_running {
            self.halt(Duration::from_millis(100))?;
        }

        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        self.write_csr(tselect, unit_index as u32)?;
        self.write_csr(tdata1, 0)?;
        self.write_csr(tdata2, 0)?;

        if was_running {
            self.resume_core()?;
        }

        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        &RISCV_CORE_REGISTERS
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
        self.state.hw_breakpoints_enabled
    }

    fn debug_on_sw_breakpoint(&mut self, enabled: bool) -> Result<(), Error> {
        self.interface.debug_on_sw_breakpoint(enabled)?;
        Ok(())
    }

    fn architecture(&self) -> Architecture {
        Architecture::Riscv
    }

    fn core_type(&self) -> CoreType {
        CoreType::Riscv
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        let misa_value = Misa(self.read_csr(0x301)?);

        // Check if the Bit at position 2 (signifies letter C, for compressed) is set.
        if misa_value.extensions() & (1 << 2) != 0 {
            Ok(InstructionSet::RV32C)
        } else {
            Ok(InstructionSet::RV32)
        }
    }

    /// Returns the number of fpu registers defined in this register file, or `None` if there are none.
    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(self
            .registers()
            .all_registers()
            .filter(|r| r.register_has_role(crate::RegisterRole::FloatingPoint))
            .count())
    }

    fn fpu_support(&mut self) -> Result<bool, crate::error::Error> {
        // Read the extensions from the Machine ISA regiseter.
        let isa_extensions =
            Misa::from(self.read_csr(Misa::get_mmio_address() as u16)?).extensions();
        // Mask for the D(double float), F(single float) and Q(quad float) extension bits.
        let mask = (1 << 3) | (1 << 5) | (1 << 16);
        Ok(isa_extensions & mask != 0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_set(&mut self.interface)?;
        Ok(())
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.sequence.reset_catch_clear(&mut self.interface)?;
        Ok(())
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.interface.disable_debug_module()?;
        Ok(())
    }
}

impl CoreMemoryInterface for Riscv32<'_> {
    type ErrorType = Error;

    fn memory(&self) -> &dyn MemoryInterface<Self::ErrorType> {
        &self.interface
    }
    fn memory_mut(&mut self) -> &mut dyn MemoryInterface<Self::ErrorType> {
        &mut self.interface
    }
}

#[derive(Debug)]
/// Flags used to control the [`SpecificCoreState`](crate::core::SpecificCoreState) for RiscV architecture
pub struct RiscvCoreState {
    /// A flag to remember whether we want to use hw_breakpoints during stepping of the core.
    hw_breakpoints_enabled: bool,

    hw_breakpoints: Option<u32>,

    /// Whether the PC was written since we last halted. Used to avoid incrementing the PC on
    /// resume.
    pc_written: bool,

    /// The semihosting command that was decoded at the current program counter
    semihosting_command: Option<SemihostingCommand>,
}

impl RiscvCoreState {
    pub(crate) fn new() -> Self {
        Self {
            hw_breakpoints_enabled: false,
            hw_breakpoints: None,
            pc_written: false,
            semihosting_command: None,
        }
    }
}

memory_mapped_bitfield_register! {
    /// `dmcontrol` register, located at address 0x10
    pub struct Dmcontrol(u32);
    0x10, "dmcontrol",
    impl From;

    /// Requests the currently selected harts to halt.
    pub _, set_haltreq: 31;

    /// Requests the currently selected harts to resume.
    pub _, set_resumereq: 30;

    /// Requests the currently selected harts to reset. Optional.
    pub hartreset, set_hartreset: 29;

    /// Writing 1 clears the `havereset` flag of selected harts.
    pub _, set_ackhavereset: 28;

    /// Selects the definition of currently selected harts. If 1, multiple harts may be selected.
    pub hasel, set_hasel: 26;

    /// The lower 10 bits of the currently selected harts.
    pub hartsello, set_hartsello: 25, 16;

    /// The upper 10 bits of the currently selected harts.
    pub hartselhi, set_hartselhi: 15, 6;

    /// Writes the halt-on-reset request bit for all currently selected hart. Optional.
    pub _, set_resethaltreq: 3;

    /// Clears the halt-on-reset request bit for all currently selected harts. Optional.
    pub _, set_clrresethaltreq: 2;

    /// This bit controls the reset signal from the DM to the rest of the system.
    pub ndmreset, set_ndmreset: 1;

    /// Reset signal for the debug module.
    pub dmactive, set_dmactive: 0;
}

impl Dmcontrol {
    /// Currently selected harts
    ///
    /// Combination of the `hartselhi` and `hartsello` registers.
    pub fn hartsel(&self) -> u32 {
        self.hartselhi() << 10 | self.hartsello()
    }

    /// Set the currently selected harts
    ///
    /// This sets the `hartselhi` and `hartsello` registers.
    /// This is a 20 bit register, larger values will be truncated.
    pub fn set_hartsel(&mut self, value: u32) {
        self.set_hartsello(value & 0x3ff);
        self.set_hartselhi((value >> 10) & 0x3ff);
    }
}

memory_mapped_bitfield_register! {
    /// Readonly `dmstatus` register.
    ///
    /// Located at address 0x11
    pub struct Dmstatus(u32);
    0x11, "dmstatus",
    impl From;

    /// If 1, then there is an implicit ebreak instruction
    /// at the non-existent word immediately after the
    /// Program Buffer. This saves the debugger from
    /// having to write the ebreak itself, and allows the
    /// Program Buffer to be one word smaller.
    /// This must be 1 when progbufsize is 1.
    pub impebreak, _: 22;

    /// This field is 1 when all currently selected harts
    /// have been reset and reset has not been acknowledged for any of them.
    pub allhavereset, _: 19;

    /// This field is 1 when at least one currently selected hart
    /// has been reset and reset has not been acknowledged for any of them.
    pub anyhavereset, _: 18;

    /// This field is 1 when all currently selected harts
    /// have acknowledged their last resume request.
    pub allresumeack, _: 17;

    /// This field is 1 when any currently selected hart
    /// has acknowledged its last resume request.
    pub anyresumeack, _: 16;

    /// This field is 1 when all currently selected harts do
    /// not exist in this platform.
    pub allnonexistent, _: 15;

    /// This field is 1 when any currently selected hart
    /// does not exist in this platform.
    pub anynonexistent, _: 14;

    /// This field is 1 when all currently selected harts are unavailable.
    pub allunavail, _: 13;

    /// This field is 1 when any currently selected hart is unavailable.
    pub anyunavail, _: 12;

    /// This field is 1 when all currently selected harts are running.
    pub allrunning, _: 11;

    /// This field is 1 when any currently selected hart is running.
    pub anyrunning, _: 10;

    /// This field is 1 when all currently selected harts are halted.
    pub allhalted, _: 9;

    /// This field is 1 when any currently selected hart is halted.
    pub anyhalted, _: 8;

    /// If 0, authentication is required before using the DM.
    pub authenticated, _: 7;

    /// If 0, the authentication module is ready to process the next read/write to `authdata`.
    pub authbusy, _: 6;

    /// 1 if this Debug Module supports halt-on-reset functionality controllable by the
    /// `setresethaltreq` and `clrresethaltreq` bits.
    pub hasresethaltreq, _: 5;

    /// 1 if `confstrptr0`–`confstrptr3` hold the address of the configuration string.
    pub confstrptrvalid, _: 4;

    /// Version of the debug module.
    pub version, _: 3, 0;
}

bitfield! {
    struct Dcsr(u32);
    impl Debug;

    xdebugver, _: 31, 28;
    ebreakm, set_ebreakm: 15;
    ebreaks, set_ebreaks: 13;
    ebreaku, set_ebreaku: 12;
    stepie, set_stepie: 11;
    stopcount, set_stopcount: 10;
    stoptime, set_stoptime: 9;
    cause, set_cause: 8, 6;
    mprven, set_mprven: 4;
    nmip, _: 3;
    step, set_step: 2;
    prv, set_prv: 1,0;
}

memory_mapped_bitfield_register! {
    /// Abstract Control and Status (see 3.12.6)
    pub struct Abstractcs(u32);
    0x16, "abstractcs",
    impl From;

    progbufsize, _: 28, 24;
    busy, _: 12;
    cmderr, set_cmderr: 10, 8;
    datacount, _: 3, 0;
}

memory_mapped_bitfield_register! {
    /// Hart Info (see 3.12.3)
    pub struct Hartinfo(u32);
    0x12, "hartinfo",
    impl From;

    nscratch, _: 23, 20;
    dataaccess, _: 16;
    datasize, _: 15, 12;
    dataaddr, _: 11, 0;
}

memory_mapped_bitfield_register! { pub struct Data0(u32); 0x04, "data0", impl From; }
memory_mapped_bitfield_register! { pub struct Data1(u32); 0x05, "data1", impl From; }
memory_mapped_bitfield_register! { pub struct Data2(u32); 0x06, "data2", impl From; }
memory_mapped_bitfield_register! { pub struct Data3(u32); 0x07, "data3", impl From; }
memory_mapped_bitfield_register! { pub struct Data4(u32); 0x08, "data4", impl From; }
memory_mapped_bitfield_register! { pub struct Data5(u32); 0x09, "data5", impl From; }
memory_mapped_bitfield_register! { pub struct Data6(u32); 0x0A, "data6", impl From; }
memory_mapped_bitfield_register! { pub struct Data7(u32); 0x0B, "data7", impl From; }
memory_mapped_bitfield_register! { pub struct Data8(u32); 0x0C, "data8", impl From; }
memory_mapped_bitfield_register! { pub struct Data9(u32); 0x0D, "data9", impl From; }
memory_mapped_bitfield_register! { pub struct Data10(u32); 0x0E, "data10", impl From; }
memory_mapped_bitfield_register! { pub struct Data11(u32); 0x0f, "data11", impl From; }

memory_mapped_bitfield_register! { struct Command(u32); 0x17, "command", impl From; }

memory_mapped_bitfield_register! { pub struct Progbuf0(u32); 0x20, "progbuf0", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf1(u32); 0x21, "progbuf1", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf2(u32); 0x22, "progbuf2", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf3(u32); 0x23, "progbuf3", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf4(u32); 0x24, "progbuf4", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf5(u32); 0x25, "progbuf5", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf6(u32); 0x26, "progbuf6", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf7(u32); 0x27, "progbuf7", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf8(u32); 0x28, "progbuf8", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf9(u32); 0x29, "progbuf9", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf10(u32); 0x2A, "progbuf10", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf11(u32); 0x2B, "progbuf11", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf12(u32); 0x2C, "progbuf12", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf13(u32); 0x2D, "progbuf13", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf14(u32); 0x2E, "progbuf14", impl From; }
memory_mapped_bitfield_register! { pub struct Progbuf15(u32); 0x2F, "progbuf15", impl From; }

bitfield! {
    struct Mcontrol(u32);
    impl Debug;

    type_, set_type: 31, 28;
    dmode, set_dmode: 27;
    maskmax, _: 26, 21;
    hit, set_hit: 20;
    select, set_select: 19;
    timing, set_timing: 18;
    sizelo, set_sizelo: 17, 16;
    action, set_action: 15, 12;
    chain, set_chain: 11;
    match_, set_match: 10, 7;
    m, set_m: 6;
    s, set_s: 4;
    u, set_u: 3;
    execute, set_execute: 2;
    store, set_store: 1;
    load, set_load: 0;
}

memory_mapped_bitfield_register! {
    /// Isa and Extensions (see RISC-V Privileged Spec, 3.1.1)
    pub struct Misa(u32);
    0x301, "misa",
    impl From;

    /// Machine XLEN
    mxl, _: 31, 30;
    /// Standard RISC-V extensions
    extensions, _: 25, 0;
}
