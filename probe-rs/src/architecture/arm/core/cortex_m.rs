//! Common functions and data types for Cortex-M core variants

use crate::{
    architecture::arm::{memory::adi_v5_memory_interface::ArmProbe, ArmError},
    core::RegisterId,
    memory_mapped_bitfield_register,
    semihosting::decode_semihosting_syscall,
    CoreInterface, Error, MemoryMappedRegister, SemihostingCommand,
};
use std::time::{Duration, Instant};

memory_mapped_bitfield_register! {
    pub struct Vtor(u32);
    0xE000_ED08, "VTOR",
    impl From;
    /// This fields holds bits [31:7] of the table offset.
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
    // If the processor does not implement the FP extension the REGSEL field is bits [4:0], and bits [6:5] are Reserved, SBZ.
    pub _, set_regsel: 6,0;
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
    pub fpu_privilige, _: 21,20;
}

impl Cpacr {
    pub fn fpu_present(&self) -> bool {
        self.fpu_privilige() != 0
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

pub enum MProgrammersModel {
    TwoStack,
    Reserved,
}

impl From<u8> for MProgrammersModel {
    fn from(value: u8) -> Self {
        match value {
            0b0010 => MProgrammersModel::TwoStack,
            _ => MProgrammersModel::Reserved,
        }
    }
}

impl From<u32> for MProgrammersModel {
    fn from(value: u32) -> Self {
        match value {
            0b0010 => MProgrammersModel::TwoStack,
            _ => MProgrammersModel::Reserved,
        }
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

pub(crate) fn read_core_reg(memory: &mut dyn ArmProbe, addr: RegisterId) -> Result<u32, ArmError> {
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
    memory: &mut dyn ArmProbe,
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
    let pc: u32 = core.read_core_reg(core.program_counter().id)?.try_into()?;

    // The Arm Semihosting Specification, specificies that the instruction
    // "BKPT 0xAB" (encoded as 0xBEAB) triggers a semihosting call.
    // <https://github.com/ARM-software/abi-aa/blob/main/semihosting/semihosting.rst#the-semihosting-interface>
    const TRAP_INSTRUCTION: [u8; 2] = [
        // instruction encoded as little endian
        0xAB, 0xBE,
    ];

    let mut actual_instruction = [0u8; 2];
    core.read_8(pc as u64, &mut actual_instruction)?;
    let actual_instruction = actual_instruction.as_slice();

    tracing::debug!(
        "Semihosting check pc={pc:#x} instruction={0:#02x}{1:#02x}",
        actual_instruction[1],
        actual_instruction[0]
    );

    if TRAP_INSTRUCTION == actual_instruction {
        // BKPT 0xAB -> we are semihosting

        Ok(Some(match cached_command {
            None => {
                // We only want to decode the semihosting command once, since answering it might change some of the registers
                let r0: u32 = core.read_core_reg(RegisterId(0))?.try_into()?;
                let r1: u32 = core.read_core_reg(RegisterId(1))?.try_into()?;
                tracing::info!("Semihosting found pc={pc:#x} r0={r0:#x} r1={r1:#x}");
                decode_semihosting_syscall(core, r0, r1)?
            }
            Some(cached_command) => cached_command,
        }))
    } else {
        Ok(None)
    }
}

fn wait_for_core_register_transfer(
    memory: &mut dyn ArmProbe,
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
