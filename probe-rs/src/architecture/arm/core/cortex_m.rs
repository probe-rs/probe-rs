//! Common functions and data types for Cortex-M core variants

use crate::{
    CoreInterface, CoreStatus, Error, MemoryMappedRegister,
    architecture::arm::{ArmError, memory::ArmMemoryInterface},
    core::RegisterId,
    memory_mapped_bitfield_register,
    semihosting::SemihostingCommand,
    semihosting::decode_semihosting_syscall,
};
use std::time::{Duration, Instant};

use super::CortexMState;

memory_mapped_bitfield_register! {
    pub struct Vtor(u32);
    0xE000_ED08, "VTOR",
    impl From;
    /// This fields holds bits `[31:7]` of the table offset.
    pub tbloff, set_tbloff: 31, 7;
}

memory_mapped_bitfield_register! {
    pub struct Dhcsr(u32);
    0xE000_EDF0, "DHCSR",
    impl From;
    pub s_reset_st, _: 25;
    pub s_retire_st, _: 24;
    pub s_lockup, _: 19;
    pub s_sleep, _: 18;
    pub s_halt, _: 17;
    pub s_regrdy, _: 16;
    pub c_maskints, set_c_maskints: 3;
    pub c_step, set_c_step: 2;
    pub c_halt, set_c_halt: 1;
    pub c_debugen, set_c_debugen: 0;
}

impl Dhcsr {
    /// This function sets the bit to enable writes to this register.
    ///
    /// C1.6.3 Debug Halting Control and Status Register, DHCSR:
    /// Debug key:
    /// Software must write 0xA05F to this field to enable write accesses to bits
    /// `[15:0]`, otherwise the processor ignores the write access.
    pub fn enable_write(&mut self) {
        self.0 &= !(0xffff << 16);
        self.0 |= 0xa05f << 16;
    }
}

memory_mapped_bitfield_register! {
    pub struct Dcrsr(u32);
    0xE000_EDF4, "DCRSR",
    impl From;
    pub _, set_regwnr: 16;
    // If the processor does not implement the FP extension the REGSEL field is bits `[4:0]`, and bits `[6:5]` are Reserved, SBZ.
    // Increased to 7 bits on v8-M
    pub _, set_regsel: 7,0;
}

memory_mapped_bitfield_register! {
    pub struct Dcrdr(u32);
    0xE000_EDF8, "DCRDR",
    impl From;
}

memory_mapped_bitfield_register! {
    ///  Coprocessor Access Control Register
    pub struct Cpacr(u32);
    0xE000_ED88, "CPACR",
    impl From;
    pub fpu_privilege, _: 21,20;
}

impl Cpacr {
    pub fn fpu_present(&self) -> bool {
        self.fpu_privilege() != 0
    }
}

memory_mapped_bitfield_register! {
    ///  Media and VFP Feature Register 0
    pub struct Mvfr0(u32);
    0xE000_EF40, "MVFR0",
    impl From;
    pub fpdp, _: 11, 8;
    pub fpsp, _: 7, 4;
}

impl Mvfr0 {
    pub fn fp_present(&self) -> bool {
        self.fpdp() != 0 || self.fpsp() != 0
    }
}

pub enum SecurityExtension {
    NotImplemented,
    Implemented,
    ImplementedWithStateHandling,
    Reserved,
}

impl From<u8> for SecurityExtension {
    fn from(value: u8) -> Self {
        match value {
            0b0000 => SecurityExtension::NotImplemented,
            0b0001 => SecurityExtension::Implemented,
            0b0011 => SecurityExtension::ImplementedWithStateHandling,
            _ => SecurityExtension::Reserved,
        }
    }
}

memory_mapped_bitfield_register! {
    /// Processor Feature Register 1
    pub struct IdPfr1(u32);
    0xE000_ED44, "ID_PFR1",
    impl From;
    /// Identifies support for the M-Profile programmer's model
    pub u8, m_prog_mod, _: 11, 8;
    /// Identifies whether the Security Extension is implemented
    pub u8, security, _: 7, 4;
}

impl IdPfr1 {
    pub fn security_present(&self) -> bool {
        matches!(
            self.security().into(),
            SecurityExtension::Implemented | SecurityExtension::ImplementedWithStateHandling
        )
    }
}

pub(crate) fn read_core_reg(
    memory: &mut dyn ArmMemoryInterface,
    addr: RegisterId,
) -> Result<u32, ArmError> {
    // Write the DCRSR value to select the register we want to read.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(false); // Perform a read.
    dcrsr_val.set_regsel(addr.into()); // The address of the register to read.

    memory.write_word_32(Dcrsr::get_mmio_address(), dcrsr_val.into())?;

    wait_for_core_register_transfer(memory, Duration::from_millis(100))?;

    let value = memory.read_word_32(Dcrdr::get_mmio_address())?;

    Ok(value)
}

pub(crate) fn write_core_reg(
    memory: &mut dyn ArmMemoryInterface,
    addr: RegisterId,
    value: u32,
) -> Result<(), ArmError> {
    memory.write_word_32(Dcrdr::get_mmio_address(), value)?;

    // write the DCRSR value to select the register we want to write.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(true); // Perform a write.
    dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

    memory.write_word_32(Dcrsr::get_mmio_address(), dcrsr_val.into())?;

    wait_for_core_register_transfer(memory, Duration::from_millis(100))?;

    Ok(())
}

/// Check if the current breakpoint is a semihosting call.
///
/// Call this if you get some kind of breakpoint. Works on ARMv6-M, ARMv7-M and ARMv8-M.
pub(crate) fn check_for_semihosting(
    cached_command: Option<SemihostingCommand>,
    core: &mut dyn CoreInterface,
) -> Result<Option<SemihostingCommand>, Error> {
    // The Arm Semihosting Specification, specifies that the instruction
    // "BKPT 0xAB" (encoded as 0xBEAB) triggers a semihosting call.
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#the-semihosting-interface>
    const TRAP_INSTRUCTION: [u8; 2] = [
        // instruction encoded as little endian
        0xAB, 0xBE,
    ];

    // We only want to decode the semihosting command once, since answering it might change some of the registers
    if let Some(command) = cached_command {
        return Ok(Some(command));
    }

    let pc: u32 = core.read_core_reg(core.program_counter().id)?.try_into()?;

    let mut actual_instruction = [0u8; 2];
    core.read_8(pc as u64, &mut actual_instruction)?;
    let actual_instruction = actual_instruction.as_slice();

    tracing::debug!(
        "Semihosting check pc={pc:#x} instruction={0:#02x}{1:#02x}",
        actual_instruction[1],
        actual_instruction[0]
    );

    let command = if TRAP_INSTRUCTION == actual_instruction {
        Some(decode_semihosting_syscall(core)?)
    } else {
        None
    };

    Ok(command)
}

fn wait_for_core_register_transfer(
    memory: &mut dyn ArmMemoryInterface,
    timeout: Duration,
) -> Result<(), ArmError> {
    // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
    // (see C1-292, cortex m0 arm)
    let start = Instant::now();

    while start.elapsed() < timeout {
        let dhcsr_val = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);

        if dhcsr_val.s_regrdy() {
            return Ok(());
        }
    }
    Err(ArmError::Timeout)
}

/// Thumb `BKPT #imm` instructions use the `0xBE00` encoding prefix.
pub(crate) const THUMB_BKPT_MASK: u16 = 0xFF00;
pub(crate) const THUMB_BKPT_PREFIX: u16 = 0xBE00;

/// Low-level [`CortexMState`] and memory accessors for shared Cortex-M helpers.
pub(crate) trait CortexMStateAccess: CoreInterface {
    fn cortex_m_state(&mut self) -> &mut CortexMState;

    fn cortex_m_memory(&mut self) -> &mut dyn ArmMemoryInterface;
}

/// Skip past a breakpoint when continuing from a halt at a trap PC.
pub(crate) fn skip_breakpoint<C: CortexMStateAccess>(core: &mut C) -> Result<(), Error> {
    if !core.status()?.is_halted() {
        return Ok(());
    }

    let pc = core.read_core_reg(core.program_counter().into())?;
    let mut bytes = [0u8; 2];
    core.read_8(pc.try_into()?, &mut bytes)?;
    let insn = u16::from_le_bytes(bytes);

    let pc_in_hw_breakpoints = core.hw_breakpoints()?.contains(&pc.try_into().ok());

    if (insn & THUMB_BKPT_MASK) == THUMB_BKPT_PREFIX && !pc_in_hw_breakpoints {
        core.cortex_m_state().clear_semihosting_command();
        let mut pc = pc;
        pc.increment_address(2)?;
        core.write_core_reg(core.program_counter().into(), pc)?;
        return Ok(());
    }

    let on_breakpoint = core.cortex_m_state().halted_on_breakpoint() || pc_in_hw_breakpoints;

    if on_breakpoint {
        core.cortex_m_state().clear_semihosting_command();
        hardware_step_with_breakpoints_disabled(core)?;
    }

    Ok(())
}

/// Single-step with FPB disabled to step off a hardware breakpoint. Does not set `pending_step`.
pub(crate) fn hardware_step_with_breakpoints_disabled<C: CortexMStateAccess>(
    core: &mut C,
) -> Result<(), Error> {
    let pc_before_step = core.read_core_reg(core.program_counter().into())?;
    core.enable_breakpoints(false)?;

    let mut value = Dhcsr(0);
    value.set_c_step(true);
    value.set_c_halt(false);
    value.set_c_debugen(true);
    value.set_c_maskints(true);
    value.enable_write();

    core.cortex_m_memory()
        .write_word_32(Dhcsr::get_mmio_address(), value.into())?;
    core.cortex_m_memory().flush()?;

    // The single-step might put the core in lockup state. Lockup isn't considered "halted"
    // so we can't use `wait_for_core_halted` here.
    // So we wait for halted OR lockup, and if we entered lockup we halt.
    wait_for_halted_or_lockup(core, Duration::from_millis(100))?;

    if core.status()? == CoreStatus::LockedUp {
        core.halt(Duration::from_millis(100))?;
    }

    let mut pc_after_step = core.read_core_reg(core.program_counter().into())?;

    if pc_before_step == pc_after_step
        && !core
            .hw_breakpoints()?
            .contains(&pc_before_step.try_into().ok())
    {
        tracing::debug!(
            "Encountered a breakpoint instruction @ {}. We need to manually advance the program counter to the next instruction.",
            pc_after_step
        );
        pc_after_step.increment_address(2)?;
        core.write_core_reg(core.program_counter().into(), pc_after_step)?;
    }

    core.enable_breakpoints(true)?;
    Ok(())
}

fn wait_for_halted_or_lockup<C: CortexMStateAccess>(
    core: &mut C,
    timeout: Duration,
) -> Result<(), Error> {
    let start = Instant::now();

    while !matches!(core.status()?, CoreStatus::Halted(_) | CoreStatus::LockedUp) {
        if start.elapsed() >= timeout {
            return Err(Error::Arm(ArmError::Timeout));
        }
        std::thread::sleep(Duration::from_millis(1));
    }

    Ok(())
}
