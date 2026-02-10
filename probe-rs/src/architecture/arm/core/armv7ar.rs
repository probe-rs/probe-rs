//! Register types and the core interface for armv7-a and armv7-r

use super::{
    CortexARState,
    instructions::aarch32::{
        build_ldc, build_mcr, build_mov, build_mrc, build_mrs, build_msr, build_stc, build_vmov,
        build_vmrs,
    },
    registers::{
        aarch32::{
            AARCH32_CORE_REGISTERS, AARCH32_WITH_FP_16_CORE_REGISTERS,
            AARCH32_WITH_FP_32_CORE_REGISTERS,
        },
        cortex_m::{FP, PC, RA, SP, XPSR},
    },
};
use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType, Endian,
    InstructionSet, MemoryInterface,
    architecture::arm::{
        ArmError, DapAccess, FullyQualifiedApAddress,
        ap::{ApRegister, BD0, BD1, BD2, BD3, TAR, TAR2},
        core::armv7a_debug_regs::*,
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
    core::{CoreRegisters, MemoryMappedRegister, RegisterId, RegisterValue},
    error::Error,
    memory::{MemoryNotAlignedError, valid_32bit_address},
};
use std::{
    mem::size_of,
    sync::Arc,
    time::{Duration, Instant},
};
use zerocopy::{FromBytes, IntoBytes};

/// The maximum amount of time to wait for the core to respond
const OPERATION_TIMEOUT: Duration = Duration::from_millis(250);

/// Addresses for accessing debug registers when in banked mode
struct BankedAccess<'a> {
    /// Keep a reference to the `interface` to prevent anyone else
    /// from changing the TAR while we're doing banked operations.
    interface: &'a mut dyn DapAccess,
    ap: FullyQualifiedApAddress,
    dtrtx: u64,
    itr: u64,
    dscr: u64,
    dtrrx: u64,
}

impl<'a> BankedAccess<'a> {
    #[expect(dead_code)]
    fn set_dtrrx(&mut self, value: u32) -> Result<(), ArmError> {
        self.interface
            .write_raw_ap_register(&self.ap, self.dtrrx, value)
    }

    fn set_dtrrx_repeated(&mut self, values: &[u32]) -> Result<(), ArmError> {
        self.interface
            .write_raw_ap_register_repeated(&self.ap, self.dtrrx, values)
    }

    fn dscr(&mut self) -> Result<Dbgdscr, ArmError> {
        self.interface
            .read_raw_ap_register(&self.ap, self.dscr)
            .map(Dbgdscr::from)
    }

    fn set_dscr(&mut self, value: Dbgdscr) -> Result<(), ArmError> {
        self.interface
            .write_raw_ap_register(&self.ap, self.dscr, value.into())
    }

    fn set_itr(&mut self, value: u32) -> Result<(), ArmError> {
        self.interface
            .write_raw_ap_register(&self.ap, self.itr, value)
    }

    fn dtrtx(&mut self) -> Result<u32, ArmError> {
        self.interface.read_raw_ap_register(&self.ap, self.dtrtx)
    }

    fn dtrtx_repeated(&mut self, buf: &mut [u32]) -> Result<(), ArmError> {
        self.interface
            .read_raw_ap_register_repeated(&self.ap, self.dtrtx, buf)?;
        Ok(())
    }

    /// Operate the core in DCC Fast mode. For more information, see
    /// ARM Architecture Reference Manual ARMv7-A and ARMv7-R edition
    /// section C8.2.2.
    ///
    /// In this mode, writing to the ITR register does not immediately
    /// trigger the instruction. Instead, it waits for a read from DTRTX
    /// or a write to DTRRX. By placing an instruction with address-increment
    /// in the pipeline this way, a load or store can be retriggered
    /// repeatedly to quickly stream memory.
    fn with_dcc_fast_mode<R>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<R, ArmError>,
    ) -> Result<R, ArmError> {
        // Place DSCR in DCC Fast mode
        let mut dscr = self.dscr()?;
        dscr.set_extdccmode(2);
        self.set_dscr(dscr)?;
        let result = f(self);

        // Return DSCR back to DCC Non Blocking mode
        let mut dscr = self.dscr()?;
        dscr.set_extdccmode(0);
        self.set_dscr(dscr)?;

        result
    }
}

/// Errors for the ARMv7-A state machine
#[derive(thiserror::Error, Debug)]
pub enum Armv7arError {
    /// Invalid register number
    #[error("Register number {0} is not valid for ARMv7-A")]
    InvalidRegisterNumber(u16),

    /// Not halted
    #[error("Core is running but operation requires it to be halted")]
    NotHalted,

    /// Data Abort occurred
    #[error("A data abort occurred")]
    DataAbort,
}

/// Interface for interacting with an ARMv7-A/R core
pub struct Armv7ar<'probe> {
    memory: Box<dyn ArmMemoryInterface + 'probe>,

    state: &'probe mut CortexARState,

    base_address: u64,

    sequence: Arc<dyn ArmDebugSequence>,

    num_breakpoints: Option<u32>,

    itr_enabled: bool,

    endianness: Option<Endian>,

    core_type: CoreType,
}

impl<'probe> Armv7ar<'probe> {
    pub(crate) fn new(
        mut memory: Box<dyn ArmMemoryInterface + 'probe>,
        state: &'probe mut CortexARState,
        base_address: u64,
        sequence: Arc<dyn ArmDebugSequence>,
        core_type: CoreType,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let address = Dbgdscr::get_mmio_address_from_base(base_address)?;
            let dbgdscr = Dbgdscr(memory.read_word_32(address)?);

            tracing::debug!("State when connecting: {:x?}", dbgdscr);

            let core_state = if dbgdscr.halted() {
                let reason = dbgdscr.halt_reason();

                tracing::debug!("Core was halted when connecting, reason: {:?}", reason);

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            state.current_state = core_state;
        }

        let mut core = Self {
            memory,
            state,
            base_address,
            sequence,
            num_breakpoints: None,
            itr_enabled: false,
            endianness: None,
            core_type,
        };

        if !core.state.initialized() {
            core.reset_register_cache();
            core.read_fp_reg_count()?;
            core.state.initialize();
        }

        Ok(core)
    }

    fn read_fp_reg_count(&mut self) -> Result<(), Error> {
        if self.state.fp_reg_count == 0 && matches!(self.state.current_state, CoreStatus::Halted(_))
        {
            self.prepare_r0_for_clobber()?;

            // Check CP10/CP11 in CPACR which indicate whether the FPU is enabled;
            // if it's disabled (both 0) then don't try to read MVFR0 as it would fault.
            let instruction = build_mrc(15, 0, 0, 1, 0, 2);
            self.execute_instruction(instruction)?;
            let instruction = build_mcr(14, 0, 0, 0, 5, 0);
            let cpacr = Cpacr(self.execute_instruction_with_result(instruction)?);
            if cpacr.cp(10) == 0 || cpacr.cp(11) == 0 {
                return Ok(());
            }

            // VMRS r0, MVFR0
            let instruction = build_vmrs(0, 0b0111);
            self.execute_instruction(instruction)?;

            // Read from r0
            let instruction = build_mcr(14, 0, 0, 0, 5, 0);
            let vmrs = self.execute_instruction_with_result(instruction)?;

            self.state.fp_reg_count = match vmrs & 0b111 {
                0b001 => 16,
                0b010 => 32,
                _ => 0,
            };
        }

        Ok(())
    }

    /// Execute an instruction
    fn execute_instruction(&mut self, instruction: u32) -> Result<Dbgdscr, ArmError> {
        if !self.state.current_state.is_halted() {
            return Err(ArmError::CoreNotHalted);
        }

        // Enable ITR if needed
        if !self.itr_enabled {
            let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
            let mut dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            dbgdscr.set_itren(true);

            self.memory.write_word_32(address, dbgdscr.into())?;

            self.itr_enabled = true;
        }

        execute_instruction(&mut *self.memory, self.base_address, instruction)
    }

    /// Execute an instruction on the CPU and return the result
    fn execute_instruction_with_result(&mut self, instruction: u32) -> Result<u32, Error> {
        // Run instruction
        let mut dbgdscr = self.execute_instruction(instruction)?;

        // Wait for TXfull
        let start = Instant::now();
        while !dbgdscr.txfull_l() {
            let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
            dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            // Check if we had any aborts, if so clear them and fail
            check_and_clear_data_abort(&mut *self.memory, self.base_address, dbgdscr)?;
            if start.elapsed() >= OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
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
        let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
        let mut dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);

        let start = Instant::now();
        while !dbgdscr.rxfull_l() {
            dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            // Check if we had any aborts, if so clear them and fail
            check_and_clear_data_abort(&mut *self.memory, self.base_address, dbgdscr)?;
            if start.elapsed() >= OPERATION_TIMEOUT {
                return Err(Error::Timeout);
            }
        }

        // Run instruction
        self.execute_instruction(instruction)?;

        Ok(())
    }

    fn reset_register_cache(&mut self) {
        self.state.register_cache = vec![None; 51];
    }

    /// Sync any updated registers back to the core
    fn writeback_registers(&mut self) -> Result<(), Error> {
        let writeback_iter = (17u16..=48).chain(15u16..=16).chain(0u16..=14);

        for i in writeback_iter {
            if let Some((val, writeback)) = self.state.register_cache[i as usize]
                && writeback
            {
                match i {
                    0..=14 => {
                        let instruction = build_mrc(14, 0, i, 0, 5, 0);

                        self.execute_instruction_with_input(instruction, val.try_into()?)?;
                    }
                    15 => {
                        // Move val to r0
                        let instruction = build_mrc(14, 0, 0, 0, 5, 0);

                        self.execute_instruction_with_input(instruction, val.try_into()?)?;

                        // Use `mov pc, r0` rather than `bx r0` because the `bx` instruction is
                        // `UNPREDICTABLE` in the debug state (ARM Architecture Reference Manual,
                        // ARMv7-A and ARMv7-R edition, C5.3: "Executing instructions in Debug state").
                        let instruction = build_mov(15, 0);
                        self.execute_instruction(instruction)?;
                    }
                    16 => {
                        // msr cpsr_fsxc, r0
                        let instruction = build_msr(0);
                        self.execute_instruction_with_input(instruction, val.try_into()?)?;
                    }
                    17..=48 => {
                        // Move value to r0, r1
                        let value: u64 = val.try_into()?;
                        let low_word = value as u32;
                        let high_word = (value >> 32) as u32;

                        let instruction = build_mrc(14, 0, 0, 0, 5, 0);
                        self.execute_instruction_with_input(instruction, low_word)?;

                        let instruction = build_mrc(14, 0, 1, 0, 5, 0);
                        self.execute_instruction_with_input(instruction, high_word)?;

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

        self.reset_register_cache();

        Ok(())
    }

    /// Save r0 if needed before it gets clobbered by instruction execution
    fn prepare_r0_for_clobber(&mut self) -> Result<(), Error> {
        self.prepare_for_clobber(0)
    }

    /// Save `r<n>` if needed before it gets clobbered by instruction execution
    fn prepare_for_clobber(&mut self, reg: usize) -> Result<(), Error> {
        if self.state.register_cache[reg].is_none() {
            // cache reg since we're going to clobber it
            let val: u32 = self.read_core_reg(RegisterId(reg as u16))?.try_into()?;

            // Mark reg as needing writeback
            self.state.register_cache[reg] = Some((val.into(), true));
        }

        Ok(())
    }

    fn set_r0(&mut self, value: u32) -> Result<(), Error> {
        let instruction = build_mrc(14, 0, 0, 0, 5, 0);

        self.execute_instruction_with_input(instruction, value)
    }

    fn set_core_status(&mut self, new_status: CoreStatus) {
        super::update_core_status(&mut self.memory, &mut self.state.current_state, new_status);
    }

    pub(crate) fn halted_access<R>(
        &mut self,
        op: impl FnOnce(&mut Self) -> Result<R, Error>,
    ) -> Result<R, Error> {
        let was_running = !(self.state.current_state.is_halted() || self.status()?.is_halted());

        if was_running {
            self.halt(Duration::from_millis(100))?;
        }

        let result = op(self);

        if was_running {
            self.run()?
        }

        result
    }

    /// For greater performance, place DBGDTRTX, DBGDTRRX, DBGITR, and DBGDCSR
    /// into the banked register window. This will allow us to directly access
    /// these four values.
    fn banked_access(&mut self) -> Result<BankedAccess<'_>, Error> {
        let address = Dbgdtrrx::get_mmio_address_from_base(self.base_address)?;
        let ap = self.memory.fully_qualified_address();
        let is_64_bit = self.is_64_bit();
        let interface = self.memory.get_arm_debug_interface()?;

        if is_64_bit {
            interface.write_raw_ap_register(&ap, TAR2::ADDRESS, (address >> 32) as u32)?;
        }
        interface.write_raw_ap_register(&ap, TAR::ADDRESS, address as u32)?;

        Ok(BankedAccess {
            interface,
            ap,
            dtrrx: BD0::ADDRESS,
            itr: BD1::ADDRESS,
            dscr: BD2::ADDRESS,
            dtrtx: BD3::ADDRESS,
        })
    }
}

// These helper functions allow access to the ARMv7-A/R core from Sequences.
// They are also used by the `CoreInterface` to avoid code duplication.

/// Request the core to halt. Does not wait for the core to halt.
pub(crate) fn request_halt(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
) -> Result<(), ArmError> {
    let address = Dbgdrcr::get_mmio_address_from_base(base_address)?;
    let mut value = Dbgdrcr(0);
    value.set_hrq(true);

    memory.write_word_32(address, value.into())?;
    Ok(())
}

/// Start the core running. This does not flush any state.
pub(crate) fn run(memory: &mut dyn ArmMemoryInterface, base_address: u64) -> Result<(), ArmError> {
    let address = Dbgdrcr::get_mmio_address_from_base(base_address)?;
    let mut value = Dbgdrcr(0);
    value.set_rrq(true);

    memory.write_word_32(address, value.into())?;

    // Wait for ack
    let address = Dbgdscr::get_mmio_address_from_base(base_address)?;

    let start = Instant::now();
    loop {
        let dbgdscr = Dbgdscr(memory.read_word_32(address)?);
        if dbgdscr.restarted() {
            return Ok(());
        }
        if start.elapsed() > OPERATION_TIMEOUT {
            return Err(ArmError::Timeout);
        }
    }
}

/// Wait for the core to be halted. If the core does not halt, then
/// this will return `ArmError::Timeout`.
pub(crate) fn wait_for_core_halted(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    timeout: Duration,
) -> Result<(), ArmError> {
    // Wait until halted state is active again.
    let start = Instant::now();

    while !core_halted(memory, base_address)? {
        if start.elapsed() >= timeout {
            return Err(ArmError::Timeout);
        }
        // Wait a bit before polling again.
        std::thread::sleep(Duration::from_millis(1));
    }

    Ok(())
}

/// Return whether or not the core is halted.
pub(crate) fn core_halted(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
) -> Result<bool, ArmError> {
    let address = Dbgdscr::get_mmio_address_from_base(base_address)?;
    let dbgdscr = Dbgdscr(memory.read_word_32(address)?);

    Ok(dbgdscr.halted())
}

/// Set and enable a specific breakpoint. If the breakpoint is in use, it
/// will be cleared.
pub(crate) fn set_hw_breakpoint(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    bp_unit_index: usize,
    addr: u32,
) -> Result<(), ArmError> {
    let bp_value_addr = Dbgbvr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;
    let bp_control_addr = Dbgbcr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;
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

    memory.write_word_32(bp_value_addr, addr)?;
    memory.write_word_32(bp_control_addr, bp_control.into())?;

    Ok(())
}

/// If a specified breakpoint is set, disable it and clear it.
pub(crate) fn clear_hw_breakpoint(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    bp_unit_index: usize,
) -> Result<(), ArmError> {
    let bp_value_addr = Dbgbvr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;
    let bp_control_addr = Dbgbcr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;

    memory.write_word_32(bp_value_addr, 0)?;
    memory.write_word_32(bp_control_addr, 0)?;
    Ok(())
}

/// Get a specific hardware breakpoint. If the breakpoint is not set, return `None`.
pub(crate) fn get_hw_breakpoint(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    bp_unit_index: usize,
) -> Result<Option<u32>, ArmError> {
    let bp_value_addr = Dbgbvr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;
    let bp_value = memory.read_word_32(bp_value_addr)?;

    let bp_control_addr = Dbgbcr::get_mmio_address_from_base(base_address)?
        + (bp_unit_index * size_of::<u32>()) as u64;
    let bp_control = Dbgbcr(memory.read_word_32(bp_control_addr)?);

    Ok(if bp_control.e() { Some(bp_value) } else { None })
}

fn check_and_clear_data_abort(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    dbgdscr: Dbgdscr,
) -> Result<(), ArmError> {
    // Check if we had any aborts, if so clear them and fail
    if dbgdscr.adabort_l() || dbgdscr.sdabort_l() || dbgdscr.und_l() {
        let address = Dbgdrcr::get_mmio_address_from_base(base_address)?;
        let mut dbgdrcr = Dbgdrcr(0);
        dbgdrcr.set_cse(true);

        memory.write_word_32(address, dbgdrcr.into())?;
        return Err(ArmError::Armv7ar(
            crate::architecture::arm::armv7ar::Armv7arError::DataAbort,
        ));
    }
    Ok(())
}

/// Execute a single instruction.
fn execute_instruction(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    instruction: u32,
) -> Result<Dbgdscr, ArmError> {
    // Run instruction
    let address = Dbgitr::get_mmio_address_from_base(base_address)?;
    memory.write_word_32(address, instruction)?;

    // Wait for completion
    let address = Dbgdscr::get_mmio_address_from_base(base_address)?;
    let mut dbgdscr = Dbgdscr(memory.read_word_32(address)?);

    let start = Instant::now();
    while !dbgdscr.instrcoml_l() {
        dbgdscr = Dbgdscr(memory.read_word_32(address)?);
        // Check if we had any aborts, if so clear them and fail
        check_and_clear_data_abort(memory, base_address, dbgdscr)?;
        if start.elapsed() >= OPERATION_TIMEOUT {
            return Err(ArmError::Timeout);
        }
    }

    // Check if we had any aborts, if so clear them and fail
    check_and_clear_data_abort(memory, base_address, dbgdscr)?;

    Ok(dbgdscr)
}

/// Set the DBGDBGDTRRX register, which can be accessed with an
/// `STC p14, c5, ..., #4` instruction.
fn set_instruction_input(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    value: u32,
) -> Result<(), ArmError> {
    // Move value
    let address = Dbgdtrrx::get_mmio_address_from_base(base_address)?;
    memory.write_word_32(address, value)?;

    // Wait for RXfull
    let address = Dbgdscr::get_mmio_address_from_base(base_address)?;
    let mut dbgdscr = Dbgdscr(memory.read_word_32(address)?);

    let start = Instant::now();
    while !dbgdscr.rxfull_l() {
        dbgdscr = Dbgdscr(memory.read_word_32(address)?);
        // Check if we had any aborts, if so clear them and fail
        check_and_clear_data_abort(memory, base_address, dbgdscr)?;
        if start.elapsed() >= OPERATION_TIMEOUT {
            return Err(ArmError::Timeout);
        }
    }

    Ok(())
}

/// Return the contents of DBGDTRTX, which is set as a result of an
/// `LDC, p14, c5, ..., #4` instruction.
fn get_instruction_result(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
) -> Result<u32, ArmError> {
    // Wait for TXfull
    let address = Dbgdscr::get_mmio_address_from_base(base_address)?;
    let start = Instant::now();
    loop {
        let dbgdscr = Dbgdscr(memory.read_word_32(address)?);
        if dbgdscr.txfull_l() {
            break;
        }
        if start.elapsed() > OPERATION_TIMEOUT {
            return Err(ArmError::Timeout);
        }
    }

    // Read result
    let address = Dbgdtrtx::get_mmio_address_from_base(base_address)?;
    memory.read_word_32(address)
}

/// Write a 32-bit value to main memory. Assumes that the core is halted. Note that
/// this clobbers $r0.
pub(crate) fn write_word_32(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    address: u32,
    data: u32,
) -> Result<(), ArmError> {
    // Load address into r0
    set_instruction_input(memory, base_address, address)?;
    execute_instruction(memory, base_address, build_mrc(14, 0, 0, 0, 5, 0))?;

    // Store the value in the DBGDBGDTRRX register and store that value into RAM.
    // STC p14, c5, [r0], #4
    set_instruction_input(memory, base_address, data)?;
    execute_instruction(memory, base_address, build_stc(14, 5, 0, 4))?;
    Ok(())
}

/// Read a 32-bit value from main memory. Assumes that the core is halted. Note that
/// this clobbers $r0.
pub(crate) fn read_word_32(
    memory: &mut dyn ArmMemoryInterface,
    base_address: u64,
    address: u32,
) -> Result<u32, ArmError> {
    // Load address into r0
    set_instruction_input(memory, base_address, address)?;
    execute_instruction(memory, base_address, build_mrc(14, 0, 0, 0, 5, 0))?;

    // Execute the instruction and store the result in the DBGDTRTX register.
    // LDC p14, c5, [r0], #4
    execute_instruction(memory, base_address, build_ldc(14, 5, 0, 4))?;
    get_instruction_result(memory, base_address)
}

impl CoreInterface for Armv7ar<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        wait_for_core_halted(&mut *self.memory, self.base_address, timeout).map_err(|e| e.into())
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        core_halted(&mut *self.memory, self.base_address).map_err(|e| e.into())
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, Error> {
        // determine current state
        let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
        let dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);

        if dbgdscr.halted() {
            let reason = dbgdscr.halt_reason();

            self.set_core_status(CoreStatus::Halted(reason));

            self.read_fp_reg_count()?;

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
            request_halt(&mut *self.memory, self.base_address)?;
            self.wait_for_core_halted(timeout)?;

            // Reset our cached values
            self.reset_register_cache();
        }
        // Update core status
        let _ = self.status()?;

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

        // Disable ITRen before sending RRQ (per ARM C5.7)
        if self.itr_enabled {
            let address = Dbgdscr::get_mmio_address_from_base(self.base_address)?;
            let mut dbgdscr = Dbgdscr(self.memory.read_word_32(address)?);
            dbgdscr.set_itren(false);
            self.memory.write_word_32(address, dbgdscr.into())?;
            self.itr_enabled = false;
        }

        run(&mut *self.memory, self.base_address)?;

        // Recompute / verify current state
        self.set_core_status(CoreStatus::Running);
        let _ = self.status()?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(
            &mut *self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;

        // Reset our cached values
        self.reset_register_cache();

        // Recompute / verify current state
        self.set_core_status(CoreStatus::Running);
        let _ = self.status()?;

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_catch_set(
            &mut *self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;
        self.sequence.reset_system(
            &mut *self.memory,
            crate::CoreType::Armv7a,
            Some(self.base_address),
        )?;

        if !self.core_halted()? {
            tracing::warn!("Core not halted after reset, platform-specific setup may be required");
            tracing::warn!("Requesting halt anyway, but system may already be initialised");
            let address = Dbgdrcr::get_mmio_address_from_base(self.base_address)?;
            let mut value = Dbgdrcr(0);
            value.set_hrq(true);
            self.memory.write_word_32(address, value.into())?;
        }

        self.sequence.reset_catch_clear(
            &mut *self.memory,
            crate::CoreType::Armv7a,
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
        // Save current breakpoint
        let bp_unit_index = (self.available_breakpoint_units()? - 1) as usize;
        let bp_value_addr = Dbgbvr::get_mmio_address_from_base(self.base_address)?
            + (bp_unit_index * size_of::<u32>()) as u64;
        let saved_bp_value = self.memory.read_word_32(bp_value_addr)?;

        let bp_control_addr = Dbgbcr::get_mmio_address_from_base(self.base_address)?
            + (bp_unit_index * size_of::<u32>()) as u64;
        let saved_bp_control = self.memory.read_word_32(bp_control_addr)?;

        // Set breakpoint for any change
        let current_pc: u32 = self
            .read_core_reg(self.program_counter().into())?
            .try_into()?;
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
        let pc_value = self.read_core_reg(self.program_counter().into())?;

        // get pc
        Ok(CoreInformation {
            pc: pc_value.try_into()?,
        })
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        let reg_num = address.0;

        // check cache
        if (reg_num as usize) < self.state.register_cache.len()
            && let Some(cached_result) = self.state.register_cache[reg_num as usize]
        {
            return Ok(cached_result.0);
        }

        // Generate instruction to extract register
        let result: Result<RegisterValue, Error> = match reg_num {
            0..=14 => {
                // r0-r14, valid
                // MCR p14, 0, <Rd>, c0, c5, 0 ; Write DBGDTRTXint Register
                let instruction = build_mcr(14, 0, reg_num, 0, 5, 0);

                let val = self.execute_instruction_with_result(instruction)?;

                Ok(val.into())
            }
            15 => {
                // PC, must access via r0
                self.prepare_r0_for_clobber()?;

                // MOV r0, PC
                let instruction = build_mov(0, 15);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let pra_plus_offset = self.execute_instruction_with_result(instruction)?;

                // PC returned is PC + 8
                Ok((pra_plus_offset - 8).into())
            }
            16 => {
                // CPSR, must access via r0
                self.prepare_r0_for_clobber()?;

                // MRS r0, CPSR
                let instruction = build_mrs(0);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let cpsr = self.execute_instruction_with_result(instruction)?;

                Ok(cpsr.into())
            }
            17..=48 => {
                // Access via r0, r1
                self.prepare_for_clobber(0)?;
                self.prepare_for_clobber(1)?;

                // If FPEXC.EN = 0, then these registers aren't safe to access.  Read as zero
                let fpexc: u32 = self.read_core_reg(50.into())?.try_into()?;
                if (fpexc & (1 << 30)) == 0 {
                    // Disabled
                    return Ok(0u32.into());
                }

                // VMOV r0, r1, <reg>
                let instruction = build_vmov(1, 0, 1, reg_num - 17);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let mut value = self.execute_instruction_with_result(instruction)? as u64;

                // Read from r1
                let instruction = build_mcr(14, 0, 1, 0, 5, 0);
                value |= (self.execute_instruction_with_result(instruction)? as u64) << 32;

                Ok(value.into())
            }
            49 => {
                // Access via r0
                self.prepare_for_clobber(0)?;

                // If FPEXC.EN = 0, then these registers aren't safe to access.  Read as zero
                let fpexc: u32 = self.read_core_reg(50.into())?.try_into()?;
                if (fpexc & (1 << 30)) == 0 {
                    // Disabled
                    return Ok(0u32.into());
                }

                // VMRS r0, FPSCR
                let instruction = build_vmrs(0, 1);
                self.execute_instruction(instruction)?;

                // Read from r0
                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let value = self.execute_instruction_with_result(instruction)?;

                Ok(value.into())
            }
            50 => {
                // Access via r0
                self.prepare_for_clobber(0)?;

                // VMRS r0, FPEXC
                let instruction = build_vmrs(0, 0b1000);
                self.execute_instruction(instruction)?;

                let instruction = build_mcr(14, 0, 0, 0, 5, 0);
                let value = self.execute_instruction_with_result(instruction)?;

                Ok(value.into())
            }
            _ => Err(Error::Arm(
                Armv7arError::InvalidRegisterNumber(reg_num).into(),
            )),
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

        if (reg_num as usize) >= self.state.register_cache.len() {
            return Err(Error::Arm(
                Armv7arError::InvalidRegisterNumber(reg_num).into(),
            ));
        }
        self.state.register_cache[reg_num as usize] = Some((value, true));

        Ok(())
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        if self.num_breakpoints.is_none() {
            let address = Dbgdidr::get_mmio_address_from_base(self.base_address)?;
            let dbgdidr = Dbgdidr(self.memory.read_word_32(address)?);

            self.num_breakpoints = Some(dbgdidr.brps() + 1);
        }
        Ok(self.num_breakpoints.unwrap())
    }

    /// See docs on the [`CoreInterface::hw_breakpoints`] trait
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.available_breakpoint_units()? as usize;

        for bp_unit_index in 0..num_hw_breakpoints {
            let bp_value_addr = Dbgbvr::get_mmio_address_from_base(self.base_address)?
                + (bp_unit_index * size_of::<u32>()) as u64;
            let bp_value = self.memory.read_word_32(bp_value_addr)?;

            let bp_control_addr = Dbgbcr::get_mmio_address_from_base(self.base_address)?
                + (bp_unit_index * size_of::<u32>()) as u64;
            let bp_control = Dbgbcr(self.memory.read_word_32(bp_control_addr)?);

            if bp_control.e() {
                breakpoints.push(Some(bp_value as u64));
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
        let addr = valid_32bit_address(addr)?;
        set_hw_breakpoint(&mut *self.memory, self.base_address, bp_unit_index, addr)?;
        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        clear_hw_breakpoint(&mut *self.memory, self.base_address, bp_unit_index)?;
        Ok(())
    }

    fn registers(&self) -> &'static CoreRegisters {
        match self.state.fp_reg_count {
            16 => &AARCH32_WITH_FP_16_CORE_REGISTERS,
            32 => &AARCH32_WITH_FP_32_CORE_REGISTERS,
            _ => &AARCH32_CORE_REGISTERS,
        }
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
        true
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn core_type(&self) -> CoreType {
        self.core_type
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        let cpsr: u32 = self.read_core_reg(XPSR.id())?.try_into()?;

        // CPSR bit 5 - T - Thumb mode
        match (cpsr >> 5) & 1 {
            1 => Ok(InstructionSet::Thumb2),
            _ => Ok(InstructionSet::A32),
        }
    }

    fn endianness(&mut self) -> Result<Endian, Error> {
        if let Some(endianness) = self.endianness {
            return Ok(endianness);
        }
        self.halted_access(|core| {
            let endianness = {
                let psr = TryInto::<u32>::try_into(core.read_core_reg(XPSR.id())?).unwrap();
                if psr & 1 << 9 == 0 {
                    Endian::Little
                } else {
                    Endian::Big
                }
            };
            core.endianness = Some(endianness);
            Ok(endianness)
        })
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        Ok(self.state.fp_reg_count != 0)
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        Ok(self.state.fp_reg_count)
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.halted_access(|core| {
            core.sequence.reset_catch_set(
                &mut *core.memory,
                CoreType::Armv7a,
                Some(core.base_address),
            )?;
            Ok(())
        })?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        // Clear the reset_catch bit which was set earlier.
        self.halted_access(|core| {
            core.sequence.reset_catch_clear(
                &mut *core.memory,
                CoreType::Armv7a,
                Some(core.base_address),
            )?;
            Ok(())
        })?;
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    fn debug_core_stop(&mut self) -> Result<(), Error> {
        if matches!(self.state.current_state, CoreStatus::Halted(_)) {
            // We may have clobbered registers we wrote during debugging
            // Best effort attempt to put them back before we exit debug mode
            self.writeback_registers()?;
        }

        self.sequence
            .debug_core_stop(&mut *self.memory, CoreType::Armv7a)?;

        Ok(())
    }
}

impl MemoryInterface for Armv7ar<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        false
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.halted_access(|core| {
            #[repr(align(4))]
            struct AlignedBytes([u8; 8]);
            let mut bytes = AlignedBytes([0u8; 8]);
            core.read(address, &mut bytes.0)?;
            let ret = match core.endianness()? {
                Endian::Little => u64::from_le_bytes(bytes.0),
                Endian::Big => u64::from_be_bytes(bytes.0),
            };

            Ok(ret)
        })
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.halted_access(|core| {
            let address = valid_32bit_address(address)?;

            // LDC p14, c5, [r0], #4
            let instr = build_ldc(14, 5, 0, 4);

            // Save r0
            core.prepare_r0_for_clobber()?;

            // Load r0 with the address to read from
            core.set_r0(address)?;

            // Read memory from [r0]
            core.execute_instruction_with_result(instr)
        })
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.halted_access(|core| {
            // Find the word this is in and its byte offset
            let mut byte_offset = address % 4;
            let word_start = address - byte_offset;

            // Read the word
            let data = core.read_word_32(word_start)?;

            // We do 32-bit reads, so we need to take a different field
            // if we're running on a big endian device.
            if Endian::Big == core.endianness()? {
                // TODO: This doesn't work accessing 16-bit words that are not aligned.
                if address & 1 != 0 {
                    return Err(Error::MemoryNotAligned(MemoryNotAlignedError {
                        address,
                        alignment: 2,
                    }));
                }
                byte_offset = 2 - byte_offset;
            }

            // Return the 16-bit word
            Ok((data >> (byte_offset * 8)) as u16)
        })
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.halted_access(|core| {
            // Find the word this is in and its byte offset
            let mut byte_offset = address % 4;

            let word_start = address - byte_offset;

            // Read the word
            let data = core.read_word_32(word_start)?;

            // We do 32-bit reads, so we need to take a different field
            // if we're running on a big endian device.
            if Endian::Big == core.endianness()? {
                byte_offset = 3 - byte_offset;
            }

            // Return the byte
            Ok(data.to_le_bytes()[byte_offset as usize])
        })
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.halted_access(|core| {
            for (i, word) in data.iter_mut().enumerate() {
                *word = core.read_word_64(address + ((i as u64) * 8))?;
            }

            Ok(())
        })
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.halted_access(|core| {
            let count = data.len();
            if count > 2 {
                // Save r0
                core.prepare_r0_for_clobber()?;
                core.set_r0(valid_32bit_address(address)?)?;

                let mut banked = core.banked_access()?;

                // Ignore any errors encountered here -- they will set a Data Abort
                // which we will pick up in `check_and_clear_data_abort()`
                if banked
                    .with_dcc_fast_mode(|banked| {
                        // LDC p14, c5, [r0], #4
                        banked.set_itr(build_ldc(14, 5, 0, 4))?;

                        // Throw away the first value, which is from a previous operation
                        let _ = banked.dtrtx()?;

                        // Continually write the tx register, which will auto-increment.
                        // Because reads lag by one instruction, we need to break before
                        // we read the final value. If we don't we will end up reading one
                        // extra word past the buffer, which may end up outside valid RAM.
                        banked.dtrtx_repeated(&mut data[0..count - 1])?;
                        Ok(())
                    })
                    .is_ok()
                {
                    // Grab the last value that we skipped during the main sequence.
                    // Ignore any errors here since they will generate an abort that
                    // will be caught below.
                    if let Ok(last) = banked.dtrtx()
                        && let Some(v) = data.last_mut()
                    {
                        *v = last;
                    }
                }

                // Check if we had any aborts, if so clear them and fail
                let dscr = banked.dscr()?;
                check_and_clear_data_abort(&mut *core.memory, core.base_address, dscr)?;
            } else {
                for (i, word) in data.iter_mut().enumerate() {
                    *word = core.read_word_32(address + ((i as u64) * 4))?;
                }
            }

            Ok(())
        })
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.halted_access(|core| {
            for (i, word) in data.iter_mut().enumerate() {
                *word = core.read_word_16(address + ((i as u64) * 2))?;
            }

            Ok(())
        })
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.read(address, data)
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.halted_access(|core| {
            if address.is_multiple_of(4) && data.len().is_multiple_of(4) {
                // Avoid heap allocation and copy if we don't need it.
                if let Ok((aligned_buffer, _)) =
                    <[u32]>::mut_from_prefix_with_elems(data, data.len() / 4)
                {
                    core.read_32(address, aligned_buffer)?;
                } else {
                    let mut temporary_buffer = vec![0u32; data.len() / 4];
                    core.read_32(address, &mut temporary_buffer)?;
                    data.copy_from_slice(temporary_buffer.as_bytes());
                }

                // We used 32-bit accesses, so swap the 32-bit values if necessary.
                if core.endianness()? == Endian::Big {
                    for word in data.chunks_exact_mut(4) {
                        word.reverse();
                    }
                }
            } else {
                let start_address = address & !3;
                let end_address = address + (data.len() as u64);
                let end_address = end_address + (4 - (end_address & 3));
                let start_extra_count = address as usize % 4;
                let mut buffer = vec![0u32; (end_address - start_address) as usize / 4];
                core.read_32(start_address, &mut buffer)?;
                if core.endianness()? == Endian::Big {
                    for word in buffer.iter_mut() {
                        *word = word.swap_bytes();
                    }
                }
                data.copy_from_slice(
                    &buffer.as_bytes()[start_extra_count..start_extra_count + data.len()],
                );
            }
            Ok(())
        })
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.halted_access(|core| {
            let (data_low, data_high) = match core.endianness()? {
                Endian::Little => (data as u32, (data >> 32) as u32),
                Endian::Big => ((data >> 32) as u32, data as u32),
            };

            core.write_32(address, &[data_low, data_high])
        })
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.halted_access(|core| {
            let address = valid_32bit_address(address)?;

            // STC p14, c5, [r0], #4
            let instr = build_stc(14, 5, 0, 4);

            // Save r0
            core.prepare_r0_for_clobber()?;

            // Load r0 with the address to write to
            core.set_r0(address)?;

            // Write to [r0]
            core.execute_instruction_with_input(instr, data)
        })
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.halted_access(|core| {
            // Find the word this is in and its byte offset
            let mut byte_offset = address % 4;
            let word_start = address - byte_offset;

            // We do 32-bit reads and writes, so we need to take a different field
            // if we're running on a big endian device.
            if Endian::Big == core.endianness()? {
                byte_offset = 3 - byte_offset;
            }

            // Get the current word value
            let current_word = core.read_word_32(word_start)?;
            let mut word_bytes = current_word.to_le_bytes();
            word_bytes[byte_offset as usize] = data;

            core.write_word_32(word_start, u32::from_le_bytes(word_bytes))
        })
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.halted_access(|core| {
            // Find the word this is in and its byte offset
            let mut byte_offset = address % 4;
            let word_start = address - byte_offset;

            // We do 32-bit reads and writes, so we need to take a different field
            // if we're running on a big endian device.
            if Endian::Big == core.endianness()? {
                // TODO: This doesn't work when accessing 16-bit words that are not aligned.
                if address & 1 != 0 {
                    return Err(Error::MemoryNotAligned(MemoryNotAlignedError {
                        address,
                        alignment: 2,
                    }));
                }
                byte_offset = 2 - byte_offset;
            }

            // Get the current word value
            let mut word = core.read_word_32(word_start)?;

            // patch the word into it
            word &= !(0xFFFFu32 << (byte_offset * 8));
            word |= (data as u32) << (byte_offset * 8);

            core.write_word_32(word_start, word)
        })
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.halted_access(|core| {
            for (i, word) in data.iter().enumerate() {
                core.write_word_64(address + ((i as u64) * 8), *word)?;
            }

            Ok(())
        })
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.halted_access(|core| {
            if data.len() > 2 {
                // Save r0
                core.prepare_r0_for_clobber()?;
                core.set_r0(valid_32bit_address(address)?)?;

                let mut banked = core.banked_access()?;

                banked
                    .with_dcc_fast_mode(|banked| {
                        // STC p14, c5, [r0], #4
                        banked.set_itr(build_stc(14, 5, 0, 4))?;

                        // Continually write the tx register, which will auto-increment
                        banked.set_dtrrx_repeated(data)?;
                        Ok(())
                    })
                    .ok();

                // Check if we had any aborts, if so clear them and fail
                let dscr = banked.dscr()?;
                check_and_clear_data_abort(&mut *core.memory, core.base_address, dscr)?;
            } else {
                // Slow path -- perform multiple writes
                for (i, word) in data.iter().enumerate() {
                    core.write_word_32(address + ((i as u64) * 4), *word)?;
                }
            }

            Ok(())
        })
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.halted_access(|core| {
            for (i, word) in data.iter().enumerate() {
                core.write_word_16(address + ((i as u64) * 2), *word)?;
            }

            Ok(())
        })
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.write(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.halted_access(|core| {
            let len = data.len();
            let start_extra_count = ((4 - (address % 4) as usize) % 4).min(len);
            let end_extra_count = (len - start_extra_count) % 4;
            assert!(start_extra_count < 4);
            assert!(end_extra_count < 4);

            // Fall back to slower bytewise access if it's not aligned
            if start_extra_count != 0 || end_extra_count != 0 {
                for (i, byte) in data.iter().enumerate() {
                    core.write_word_8(address + (i as u64), *byte)?;
                }
                return Ok(());
            }

            // Make sure we don't try to do an empty but potentially unaligned write
            // We do a 32 bit write of the remaining bytes that are 4 byte aligned.
            let mut buffer = vec![0u32; data.len() / 4];
            let endianness = core.endianness()?;
            for (bytes, value) in data.chunks_exact(4).zip(buffer.iter_mut()) {
                *value = match endianness {
                    Endian::Little => u32::from_le_bytes(bytes.try_into().unwrap()),
                    Endian::Big => u32::from_be_bytes(bytes.try_into().unwrap()),
                }
            }
            core.write_32(address, &buffer)?;

            Ok(())
        })
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
            FullyQualifiedApAddress, communication_interface::SwdSequence,
            sequences::DefaultArmSequence,
        },
        probe::DebugProbeError,
    };

    use super::*;

    const TEST_BASE_ADDRESS: u64 = 0x8000_1000;

    fn address_to_reg_num(address: u64) -> u32 {
        ((address - TEST_BASE_ADDRESS) / 4) as u32
    }

    #[derive(Debug)]
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

    impl MemoryInterface<ArmError> for MockProbe {
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

        fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), ArmError> {
            self.read_8(address, data)
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

            assert!(
                !expected_op.read,
                "Read/write mismatch on register: {:#}",
                address_to_reg_num(address)
            );
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

        fn write(&mut self, address: u64, data: &[u8]) -> Result<(), ArmError> {
            self.write_8(address, data)
        }

        fn flush(&mut self) -> Result<(), ArmError> {
            todo!()
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

    impl ArmMemoryInterface for MockProbe {
        fn fully_qualified_address(&self) -> FullyQualifiedApAddress {
            todo!()
        }

        fn get_arm_debug_interface(
            &mut self,
        ) -> Result<&mut dyn crate::architecture::arm::ArmDebugInterface, DebugProbeError> {
            Err(DebugProbeError::NotImplemented {
                function_name: "get_arm_debug_interface",
            })
        }

        fn generic_status(&mut self) -> Result<crate::architecture::arm::ap::CSW, ArmError> {
            Err(ArmError::Probe(DebugProbeError::NotImplemented {
                function_name: "generic_status",
            }))
        }

        fn base_address(&mut self) -> Result<u64, ArmError> {
            todo!()
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
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(halted);
        dbgdscr.set_restarted(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
    }

    fn add_enable_itr_expectations(probe: &mut MockProbe) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        dbgdscr.set_itren(true);
        probe.expected_write(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
    }

    fn add_read_reg_expectations(probe: &mut MockProbe, reg: u16, value: u32) {
        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_mcr(14, 0, reg, 0, 5, 0),
        );
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_txfull_l(true);

        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
    }

    fn add_read_pc_expectations(probe: &mut MockProbe, value: u32) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_txfull_l(true);

        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_mov(0, 15),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        // + 8 to add expected offset on halt
        add_read_reg_expectations(probe, 0, value + 8);
    }

    fn add_read_fp_count_expectations(probe: &mut MockProbe) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_txfull_l(true);
        let mut cpacr = Cpacr(0);
        cpacr.set_cp(10, 0b11);
        cpacr.set_cp(11, 0b11);

        // CPACR read: MRC
        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_mrc(15, 0, 0, 1, 0, 2),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        // CPACR read: MCR
        add_read_reg_expectations(probe, 0, cpacr.0);

        // MVFR0 read
        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_vmrs(0, 0b0111),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        add_read_reg_expectations(probe, 0, 0b010);
    }

    fn add_read_cpsr_expectations(probe: &mut MockProbe, value: u32) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_txfull_l(true);

        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_mrs(0),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        add_read_reg_expectations(probe, 0, value);
    }

    fn add_idr_expectations(probe: &mut MockProbe, bp_count: u32) {
        let mut dbgdidr = Dbgdidr(0);
        dbgdidr.set_brps(bp_count - 1);
        probe.expected_read(
            Dbgdidr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdidr.into(),
        );
    }

    fn add_set_r0_expectation(probe: &mut MockProbe, value: u32) {
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_rxfull_l(true);

        probe.expected_write(
            Dbgdtrrx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_mrc(14, 0, 0, 0, 5, 0),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
    }

    fn add_read_memory_expectations(probe: &mut MockProbe, address: u64, value: u32) {
        add_set_r0_expectation(probe, address as u32);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_instrcoml_l(true);
        dbgdscr.set_txfull_l(true);

        probe.expected_write(
            Dbgitr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            build_ldc(14, 5, 0, 4),
        );
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        probe.expected_read(
            Dbgdtrtx::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            value,
        );
    }

    impl Drop for MockProbe {
        fn drop(&mut self) {
            if !self.expected_ops.is_empty() {
                panic!("self.expected_ops is not empty: {:?}", self.expected_ops);
            }
        }
    }

    #[test]
    fn armv7a_new() {
        let mut probe = MockProbe::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        let mock_mem = Box::new(probe) as _;

        let _ = Armv7ar::new(
            mock_mem,
            &mut CortexARState::new(),
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();
    }

    #[test]
    fn armv7a_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        dbgdscr.set_halted(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // First read false, second read true
        assert!(!armv7ar.core_halted().unwrap());
        assert!(armv7ar.core_halted().unwrap());
    }

    #[test]
    fn armv7a_wait_for_core_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        dbgdscr.set_halted(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // Should halt on second read
        armv7ar
            .wait_for_core_halted(Duration::from_millis(100))
            .unwrap();
    }

    #[test]
    fn armv7a_status_running() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(false);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // Should halt on second read
        assert_eq!(CoreStatus::Running, armv7ar.status().unwrap());
    }

    #[test]
    fn armv7a_status_halted() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_halted(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // Should halt on second read
        assert_eq!(
            CoreStatus::Halted(crate::HaltReason::Request),
            armv7ar.status().unwrap()
        );
    }

    #[test]
    fn armv7a_read_core_reg_common() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read register
        add_read_reg_expectations(&mut probe, 2, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(RegisterId(2)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(RegisterId(2)).unwrap()
        );
    }

    #[test]
    fn armv7a_read_core_reg_pc() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read PC
        add_read_pc_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(RegisterId(15)).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(RegisterId(15)).unwrap()
        );
    }

    #[test]
    fn armv7a_read_core_reg_cpsr() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read CPSR
        add_read_cpsr_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // First read will hit expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(XPSR.id()).unwrap()
        );

        // Second read will cache, no new expectations
        assert_eq!(
            RegisterValue::from(REG_VALUE),
            armv7ar.read_core_reg(XPSR.id()).unwrap()
        );
    }

    #[test]
    fn armv7a_halt() {
        const REG_VALUE: u32 = 0xABCD;

        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, false);

        // Write halt request
        let mut dbgdrcr = Dbgdrcr(0);
        dbgdrcr.set_hrq(true);
        probe.expected_write(
            Dbgdrcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdrcr.into(),
        );

        // Wait for halted
        add_status_expectations(&mut probe, true);

        // Read status
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read PC
        add_read_pc_expectations(&mut probe, REG_VALUE);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        // Verify PC
        assert_eq!(
            REG_VALUE as u64,
            armv7ar.halt(Duration::from_millis(100)).unwrap().pc
        );
    }

    #[test]
    fn armv7a_run() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Writeback r0
        add_set_r0_expectation(&mut probe, 0);

        // Disable ITRen
        let mut dbgdscr = Dbgdscr(0);
        dbgdscr.set_itren(true);
        probe.expected_read(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );
        dbgdscr.set_itren(false);
        probe.expected_write(
            Dbgdscr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdscr.into(),
        );

        // Write resume request
        let mut dbgdrcr = Dbgdrcr(0);
        dbgdrcr.set_rrq(true);
        probe.expected_write(
            Dbgdrcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgdrcr.into(),
        );

        // Wait for running
        add_status_expectations(&mut probe, false);

        // Read status
        add_status_expectations(&mut probe, false);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        armv7ar.run().unwrap();
    }

    #[test]
    fn armv7a_available_breakpoint_units() {
        const BP_COUNT: u32 = 4;
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        assert_eq!(BP_COUNT, armv7ar.available_breakpoint_units().unwrap());
    }

    #[test]
    fn armv7a_hw_breakpoints() {
        const BP_COUNT: u32 = 4;
        const BP1: u64 = 0x2345;
        const BP2: u64 = 0x8000_0000;
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read breakpoint count
        add_idr_expectations(&mut probe, BP_COUNT);

        // Read BP values and controls
        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            BP1 as u32,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            1,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4,
            BP2 as u32,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + 4,
            1,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (2 * 4),
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (2 * 4),
            0,
        );

        probe.expected_read(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (3 * 4),
            0,
        );
        probe.expected_read(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap() + (3 * 4),
            0,
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        let results = armv7ar.hw_breakpoints().unwrap();
        assert_eq!(Some(BP1), results[0]);
        assert_eq!(Some(BP2), results[1]);
        assert_eq!(None, results[2]);
        assert_eq!(None, results[3]);
    }

    #[test]
    fn armv7a_set_hw_breakpoint() {
        const BP_VALUE: u64 = 0x2345;
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

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
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            dbgbcr.into(),
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        armv7ar.set_hw_breakpoint(0, BP_VALUE).unwrap();
    }

    #[test]
    fn armv7a_clear_hw_breakpoint() {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Update BP value and control
        probe.expected_write(
            Dbgbvr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            0,
        );
        probe.expected_write(
            Dbgbcr::get_mmio_address_from_base(TEST_BASE_ADDRESS).unwrap(),
            0,
        );

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        armv7ar.clear_hw_breakpoint(0).unwrap();
    }

    #[test]
    fn armv7a_read_word_32() {
        const MEMORY_VALUE: u32 = 0xBA5EBA11;
        const MEMORY_ADDRESS: u64 = 0x12345678;

        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read memory
        add_read_memory_expectations(&mut probe, MEMORY_ADDRESS, MEMORY_VALUE);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();

        assert_eq!(MEMORY_VALUE, armv7ar.read_word_32(MEMORY_ADDRESS).unwrap());
    }

    fn test_read_word(value: u32, address: u64, memory_word_address: u64, endian: Endian) -> u8 {
        let mut probe = MockProbe::new();
        let mut state = CortexARState::new();

        // Add expectations
        add_status_expectations(&mut probe, true);
        add_enable_itr_expectations(&mut probe);
        add_read_reg_expectations(&mut probe, 0, 0);
        add_read_fp_count_expectations(&mut probe);

        // Read memory
        add_read_memory_expectations(&mut probe, memory_word_address, value);

        // Set endianx
        let cpsr = if endian == Endian::Big { 1 << 9 } else { 0 };
        add_read_cpsr_expectations(&mut probe, cpsr);

        let mock_mem = Box::new(probe) as _;

        let mut armv7ar = Armv7ar::new(
            mock_mem,
            &mut state,
            TEST_BASE_ADDRESS,
            DefaultArmSequence::create(),
            CoreType::Armv7a,
        )
        .unwrap();
        armv7ar.read_word_8(address).unwrap()
    }

    #[test]
    fn armv7a_read_word_8() {
        const MEMORY_VALUE: u32 = 0xBA5EBB11;
        const MEMORY_ADDRESS: u64 = 0x12345679;
        const MEMORY_WORD_ADDRESS: u64 = 0x12345678;

        assert_eq!(
            0xBB,
            test_read_word(
                MEMORY_VALUE,
                MEMORY_ADDRESS,
                MEMORY_WORD_ADDRESS,
                Endian::Little
            )
        );
    }

    #[test]
    fn armv7a_read_word_8_be() {
        const MEMORY_VALUE: u32 = 0xBA5EBB11;
        const MEMORY_ADDRESS: u64 = 0x12345679;
        const MEMORY_WORD_ADDRESS: u64 = 0x12345678;

        assert_eq!(
            0x5E,
            test_read_word(
                MEMORY_VALUE,
                MEMORY_ADDRESS,
                MEMORY_WORD_ADDRESS,
                Endian::Big
            )
        );
    }
}
