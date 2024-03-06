//! All the interface bits for RISC-V.

#![allow(clippy::inconsistent_digit_grouping)]

use crate::{
    core::{
        Architecture, BreakpointCause, CoreInformation, CoreRegisters, RegisterId, RegisterValue,
    },
    memory::valid_32bit_address,
    memory_mapped_bitfield_register,
    probe::DebugProbeError,
    semihosting::decode_semihosting_syscall,
    CoreInterface, CoreRegister, CoreStatus, CoreType, Error, HaltReason, InstructionSet,
    MemoryInterface, MemoryMappedRegister,
};
use anyhow::{anyhow, Result};
use bitfield::bitfield;
use communication_interface::{AbstractCommandErrorKind, RiscvCommunicationInterface, RiscvError};
use registers::{FP, RA, RISCV_CORE_REGSISTERS, SP};
use std::time::{Duration, Instant};

#[macro_use]
pub(crate) mod registers;
pub use registers::PC;
pub(crate) mod assembly;
pub mod communication_interface;
pub(crate) mod dtm;
pub mod sequences;

/// An interface to operate RISC-V cores.
pub struct Riscv32<'probe> {
    hart: u32,
    interface: &'probe mut RiscvCommunicationInterface,
    state: &'probe mut RiscVState,
}

impl<'probe> Riscv32<'probe> {
    /// Create a new RISC-V interface.
    pub fn new(
        hart: u32,
        interface: &'probe mut RiscvCommunicationInterface,
        state: &'probe mut RiscVState,
    ) -> Result<Self, RiscvError> {
        if !interface.hart_enabled(hart) {
            return Err(RiscvError::HartUnavailable);
        }

        Ok(Self {
            hart,
            interface,
            state,
        })
    }

    fn read_csr(&mut self, address: u16) -> Result<u32, RiscvError> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_csr(address)
    }

    fn write_csr(&mut self, address: u16, value: u32) -> Result<(), RiscvError> {
        self.interface.select_hart(self.hart)?;

        tracing::debug!("Writing CSR {:#x}", address);

        match self.interface.abstract_cmd_register_write(address, value) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                tracing::debug!("Could not write core register {:#x} with abstract command, falling back to program buffer", address);
                self.interface.write_csr_progbuf(address, value)
            }
            other => other,
        }
    }

    // Resume the core.
    fn resume_core(&mut self) -> Result<(), crate::Error> {
        self.interface.select_hart(self.hart)?;

        // set resume request.
        let mut dmcontrol: Dmcontrol = self.interface.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        self.interface.write_dm_register(dmcontrol)?;

        // check if request has been acknowleged.
        let status: Dmstatus = self.interface.read_dm_register()?;
        if !status.allresumeack() {
            return Err(RiscvError::RequestNotAcknowledged.into());
        };

        // clear resume request.
        dmcontrol.set_resumereq(false);
        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    /// Check if the connected device supports halt after reset.
    ///
    /// Returns a cached value if available, otherwise queries the
    /// `hasresethaltreq` bit in the `dmstatus` register.
    fn supports_reset_halt_req(&mut self) -> Result<bool, crate::Error> {
        self.interface.select_hart(self.hart)?;

        if let Some(has_reset_halt_req) = self.state.hasresethaltreq {
            Ok(has_reset_halt_req)
        } else {
            let dmstatus: Dmstatus = self.interface.read_dm_register()?;

            self.state.hasresethaltreq = Some(dmstatus.hasresethaltreq());

            Ok(dmstatus.hasresethaltreq())
        }
    }

    /// Check if the current breakpoint is a semihosting call
    fn check_for_semihosting(
        old_reason: HaltReason,
        core: &mut dyn CoreInterface,
    ) -> Result<HaltReason, Error> {
        let mut reason = old_reason;
        let pc: u32 = core.read_core_reg(core.program_counter().id)?.try_into()?;

        // The Riscv Semihosting Specification, specificies the following sequence of instructions,
        // to trigger a semihosting call:
        // <https://github.com/riscv-software-src/riscv-semihosting/blob/main/riscv-semihosting-spec.adoc>

        const TRAP_INSTRUCTIONS: [u32; 3] = [
            0x01f01013, // slli x0, x0, 0x1f (Entry Nop)
            0x00100073, // ebreak (Break to debugger)
            0x40705013, // srai x0, x0, 7 (NOP encoding the semihosting call number 7)
        ];

        // Read the actual instructions, starting at the instruction before the ebreak (PC-4)
        let mut actual_instructions = [0u32; 3];
        core.read_32((pc - 4) as u64, &mut actual_instructions)?;
        let actual_instructions = actual_instructions.as_slice();

        tracing::debug!(
            "Semihosting check pc={pc:#x} instructions={0:#08x} {1:#08x} {2:#08x}",
            actual_instructions[0],
            actual_instructions[1],
            actual_instructions[2]
        );

        if TRAP_INSTRUCTIONS == actual_instructions {
            // Trap sequence found -> we're semihosting
            let a0: u32 = core
                .read_core_reg(core.registers().get_argument_register(0).unwrap().id())?
                .try_into()?;
            let a1: u32 = core
                .read_core_reg(core.registers().get_argument_register(1).unwrap().id())?
                .try_into()?;

            tracing::info!("Semihosting found pc={pc:#x} a0={a0:#x} a1={a1:#x}");

            reason = HaltReason::Breakpoint(BreakpointCause::Semihosting(
                decode_semihosting_syscall(core, a0, a1)?,
            ));
        }
        Ok(reason)
    }
}

impl<'probe> CoreInterface for Riscv32<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), crate::Error> {
        self.interface.select_hart(self.hart)?;
        Ok(self.interface.wait_for_core_halted(timeout)?)
    }

    fn core_halted(&mut self) -> Result<bool, crate::Error> {
        self.interface.select_hart(self.hart)?;

        let dmstatus: Dmstatus = self.interface.read_dm_register()?;

        Ok(dmstatus.allhalted())
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, crate::Error> {
        self.interface.select_hart(self.hart)?;

        // TODO: We should use hartsum to determine if any hart is halted
        //       quickly

        let status: Dmstatus = self.interface.read_dm_register()?;

        if status.allhalted() {
            // determine reason for halt
            let dcsr = Dcsr(self.read_core_reg(RegisterId::from(0x7b0))?.try_into()?);

            let reason = match dcsr.cause() {
                // An ebreak instruction was hit
                1 => {
                    let reason = HaltReason::Breakpoint(BreakpointCause::Software);
                    Riscv32::check_for_semihosting(reason, self)?
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
            Err(
                anyhow!("Some cores are running while some are halted, this should not happen.")
                    .into(),
            )
        }
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, crate::Error> {
        self.interface.select_hart(self.hart)?;
        Ok(self.interface.halt(timeout)?)
    }

    fn run(&mut self) -> Result<(), crate::Error> {
        self.interface.select_hart(self.hart)?;

        // Before we run, we always perform a single instruction step, to account for possible breakpoints that might get us stuck on the current instruction.
        self.step()?;

        // resume the core.
        self.resume_core()?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), crate::Error> {
        self.interface.select_hart(self.hart)?;

        match self.reset_and_halt(Duration::from_millis(500)) {
            Ok(_) => self.resume_core()?,
            Err(error) => {
                return Err(RiscvError::DebugProbe(DebugProbeError::Other(
                    anyhow::anyhow!("Error during reset").context(error),
                ))
                .into());
            }
        }
        Ok(())
    }

    fn reset_and_halt(
        &mut self,
        timeout: Duration,
    ) -> Result<crate::core::CoreInformation, crate::Error> {
        self.interface.select_hart(self.hart)?;

        tracing::debug!("Resetting core, setting hartreset bit");

        let mut dmcontrol: Dmcontrol = self.interface.read_dm_register()?;
        dmcontrol.set_dmactive(true);
        dmcontrol.set_hartreset(true);
        dmcontrol.set_haltreq(true);

        self.interface.write_dm_register(dmcontrol)?;

        // Read back register to verify reset is supported
        let readback: Dmcontrol = self.interface.read_dm_register()?;

        if readback.hartreset() {
            tracing::debug!("Clearing hartreset bit");
            // Reset is performed by setting the bit high, and then low again
            let mut dmcontrol = readback;
            dmcontrol.set_dmactive(true);
            dmcontrol.set_hartreset(false);

            self.interface.write_dm_register(dmcontrol)?;
        } else {
            // Hartreset is not supported, whole core needs to be reset
            //
            // TODO: Cache this
            tracing::debug!("Hartreset bit not supported, using ndmreset");
            dmcontrol.set_hartreset(false);
            dmcontrol.set_ndmreset(true);
            dmcontrol.set_haltreq(true);

            self.interface.write_dm_register(dmcontrol)?;

            tracing::debug!("Clearing ndmreset bit");
            dmcontrol.set_ndmreset(false);
            dmcontrol.set_haltreq(true);

            self.interface.write_dm_register(dmcontrol)?;
        }

        let start = Instant::now();

        loop {
            // check that cores have reset
            let readback: Dmstatus = self.interface.read_dm_register()?;

            if readback.allhavereset() && readback.allhalted() {
                break;
            }

            if start.elapsed() > timeout {
                return Err(RiscvError::RequestNotAcknowledged.into());
            }
        }

        // acknowledge the reset, clear the halt request
        dmcontrol.set_hartreset(false);
        dmcontrol.set_ndmreset(false);
        dmcontrol.set_ackhavereset(true);

        self.interface.write_dm_register(dmcontrol)?;

        // Reenable halt on breakpoint because this gets disabled if we reset the core
        self.debug_on_sw_breakpoint(true)?; // TODO: only restore if enabled before?

        let pc = self.read_core_reg(RegisterId(0x7b1))?;

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn step(&mut self) -> Result<crate::core::CoreInformation, crate::Error> {
        self.interface.select_hart(self.hart)?;

        let halt_reason = self.status()?;
        let flashing_done = self.state.hw_breakpoints_enabled;
        if matches!(
            halt_reason,
            CoreStatus::Halted(HaltReason::Breakpoint(
                BreakpointCause::Software | BreakpointCause::Semihosting(_)
            ))
        ) && flashing_done
        {
            // If we are halted on a software breakpoint AND we have passed the flashing operation, we can skip the single step and manually advance the dpc.
            let mut debug_pc = self.read_core_reg(RegisterId(0x7b1))?;
            // Advance the dpc by the size of the EBREAK (ebreak or c.ebreak) instruction.
            if matches!(self.instruction_set()?, InstructionSet::RV32C) {
                debug_pc.increment_address(2)?;
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

        Ok(CoreInformation { pc: pc.try_into()? })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, crate::Error> {
        self.interface.select_hart(self.hart)?;

        self.read_csr(address.0)
            .map(|v| v.into())
            .map_err(|e| e.into())
    }

    fn write_core_reg(
        &mut self,
        address: RegisterId,
        value: RegisterValue,
    ) -> Result<(), crate::Error> {
        self.interface.select_hart(self.hart)?;

        let value: u32 = value.try_into()?;
        self.write_csr(address.0, value).map_err(|e| e.into())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, crate::Error> {
        self.interface.select_hart(self.hart)?;

        // TODO: This should probably only be done once, when initialising

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
                    other_error => return Err(other_error.into()),
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
                Err(other) => return Err(other.into()),
            }

            tselect_index += 1;
        }

        tracing::debug!("Target supports {} breakpoints.", tselect_index);

        Ok(tselect_index)
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait
    /// NOTE: For riscv, this assumes that only execution breakpoints are used.
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        // this can be called w/o halting the core via Session::new - temporarily halt if not halted
        self.interface.select_hart(self.hart)?;

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
        self.interface.select_hart(self.hart)?;

        // Loop through all triggers, and enable/disable them.
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;

        for bp_unit_index in 0..self.available_breakpoint_units()? as usize {
            // Select the trigger.
            self.write_csr(tselect, bp_unit_index as u32)?;

            // Read the trigger "configuration" data.
            let mut tdata_value = Mcontrol(self.read_csr(tdata1)?);

            // Only modify the trigger if it is for an execution debug action in all modes(probe-rs enabled it) or no modes (we previously disabled it).
            if tdata_value.type_() == 0b10
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
        self.interface.select_hart(self.hart)?;

        let addr = valid_32bit_address(addr)?;

        if !self.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        // select requested trigger
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        tracing::debug!("Setting breakpoint {}", bp_unit_index);

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
        instruction_breakpoint.set_match(0);

        instruction_breakpoint.set_m(true);

        instruction_breakpoint.set_u(true);

        // Trigger when instruction is executed
        instruction_breakpoint.set_execute(true);

        instruction_breakpoint.set_dmode(true);

        // Match address
        instruction_breakpoint.set_select(false);

        self.write_csr(tdata1, instruction_breakpoint.0)?;
        self.write_csr(tdata2, addr)?;

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), crate::Error> {
        // this can be called w/o halting the core via Session::new - temporarily halt if not halted
        self.interface.select_hart(self.hart)?;

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
        &RISCV_CORE_REGSISTERS
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

    fn debug_on_sw_breakpoint(&mut self, enabled: bool) -> Result<(), crate::error::Error> {
        self.interface.select_hart(self.hart)?;

        let mut dcsr = Dcsr(self.read_core_reg(RegisterId(0x7b0))?.try_into()?);

        dcsr.set_ebreakm(enabled);
        dcsr.set_ebreaks(enabled);
        dcsr.set_ebreaku(enabled);

        self.write_csr(0x7b0, dcsr.0).map_err(|e| e.into())
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
        self.interface.select_hart(self.hart)?;

        // Read the extensions from the Machine ISA regiseter.
        let isa_extensions =
            Misa::from(self.read_csr(Misa::get_mmio_address() as u16)?).extensions();
        // Mask for the D(double float), F(single float) and Q(quad float) extension bits.
        let mask = (1 << 3) | (1 << 5) | (1 << 16);
        Ok(isa_extensions & mask != 0)
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;

        if !self.supports_reset_halt_req()? {
            return Err(Error::Riscv(RiscvError::ResetHaltRequestNotSupported));
        }

        let mut dmcontrol: Dmcontrol = self.interface.read_dm_register()?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_resethaltreq(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;

        if !self.supports_reset_halt_req()? {
            return Err(Error::Riscv(RiscvError::ResetHaltRequestNotSupported));
        }

        let mut dmcontrol: Dmcontrol = self.interface.read_dm_register()?;

        dmcontrol.set_dmactive(true);
        dmcontrol.set_clrresethaltreq(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;

        self.debug_on_sw_breakpoint(false)?;
        Ok(())
    }
}

impl<'probe> MemoryInterface for Riscv32<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.interface.supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, crate::error::Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_word_32(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_word_16(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.read_8(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_word_32(address, data)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_word_16(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_32(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_16(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write_8(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.interface.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.interface.select_hart(self.hart)?;
        self.interface.flush()
    }
}

#[derive(Debug)]
/// Flags used to control the [`SpecificCoreState`](crate::core::SpecificCoreState) for RiscV architecture
pub struct RiscVState {
    /// A flag to remember whether we want to use hw_breakpoints during stepping of the core.
    hw_breakpoints_enabled: bool,

    /// Store the value of the `hasresethaltreq` bit of the `dmcstatus` register.
    hasresethaltreq: Option<bool>,
}

impl RiscVState {
    pub(crate) fn new() -> Self {
        Self {
            hw_breakpoints_enabled: false,
            hasresethaltreq: None,
        }
    }
}

memory_mapped_bitfield_register! {
    /// `dmcontrol` register, located at
    /// address 0x10
    pub struct Dmcontrol(u32);
    0x10, "dmcontrol",
    impl From;
    _, set_haltreq: 31;
    _, set_resumereq: 30;
    hartreset, set_hartreset: 29;
    _, set_ackhavereset: 28;
    hasel, set_hasel: 26;
    hartsello, set_hartsello: 25, 16;
    hartselhi, set_hartselhi: 15, 6;
    _, set_resethaltreq: 3;
    _, set_clrresethaltreq: 2;
    ndmreset, set_ndmreset: 1;
    dmactive, set_dmactive: 0;
}

impl Dmcontrol {
    /// Currently selected harts
    ///
    /// Combination of the `hartselhi` and `hartsello` registers.
    fn hartsel(&self) -> u32 {
        self.hartselhi() << 10 | self.hartsello()
    }

    /// Set the currently selected harts
    ///
    /// This sets the `hartselhi` and `hartsello` registers.
    /// This is a 20 bit register, larger values will be truncated.
    fn set_hartsel(&mut self, value: u32) {
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
    impebreak, _: 22;
    allhavereset, _: 19;
    anyhavereset, _: 18;
    allresumeack, _: 17;
    anyresumeack, _: 16;
    allnonexistent, _: 15;
    anynonexistent, _: 14;
    allunavail, _: 13;
    anyunavail, _: 12;
    allrunning, _: 11;
    anyrunning, _: 10;
    allhalted, _: 9;
    anyhalted, _: 8;
    authenticated, _: 7;
    authbusy, _: 6;
    hasresethaltreq, _: 5;
    confstrptrvalid, _: 4;
    version, _: 3, 0;
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
