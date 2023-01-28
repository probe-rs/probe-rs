//! Common functions and data types for Cortex-M core variants

use crate::{
    architecture::arm::{memory::adi_v5_memory_interface::ArmProbe, ArmError},
    Error, MemoryMappedRegister, RegisterId,
};

use bitfield::bitfield;
use std::time::{Duration, Instant};

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Dhcsr(u32);
    impl Debug;
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
    /// [15:0], otherwise the processor ignores the write access.
    pub fn enable_write(&mut self) {
        self.0 &= !(0xffff << 16);
        self.0 |= 0xa05f << 16;
    }
}

impl From<u32> for Dhcsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dhcsr> for u32 {
    fn from(value: Dhcsr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister for Dhcsr {
    const ADDRESS: u64 = 0xE000_EDF0;
    const NAME: &'static str = "DHCSR";
}

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Dcrsr(u32);
    impl Debug;
    pub _, set_regwnr: 16;
    // If the processor does not implement the FP extension the REGSEL field is bits [4:0], and bits [6:5] are Reserved, SBZ.
    pub _, set_regsel: 6,0;
}

impl From<u32> for Dcrsr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dcrsr> for u32 {
    fn from(value: Dcrsr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister for Dcrsr {
    const ADDRESS: u64 = 0xE000_EDF4;
    const NAME: &'static str = "DCRSR";
}

#[derive(Debug, Copy, Clone)]
pub struct Dcrdr(u32);

impl From<u32> for Dcrdr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dcrdr> for u32 {
    fn from(value: Dcrdr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister for Dcrdr {
    const ADDRESS: u64 = 0xE000_EDF8;
    const NAME: &'static str = "DCRDR";
}

bitfield! {
    ///  Coprocessor Access Control Register
    #[derive(Copy, Clone)]
    pub struct Cpacr(u32);
    impl Debug;
    pub fpu_privilige, _: 21,20;
}

impl Cpacr {
    pub fn fpu_present(&self) -> bool {
        self.fpu_privilige() != 0
    }
}

impl From<u32> for Cpacr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Cpacr> for u32 {
    fn from(value: Cpacr) -> Self {
        value.0
    }
}

impl MemoryMappedRegister for Cpacr {
    const ADDRESS: u64 = 0xE000_ED88;
    const NAME: &'static str = "CPACR";
}

bitfield! {
    ///  Media and VFP Feature Register 0
    #[derive(Copy, Clone)]
    pub struct Mvfr0(u32);
    impl Debug;
    pub fpdp, _: 11, 8;
    pub fpsp, _: 7, 4;
}

impl Mvfr0 {
    pub fn fp_present(&self) -> bool {
        self.fpdp() != 0 || self.fpsp() != 0
    }
}

impl From<u32> for Mvfr0 {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Mvfr0> for u32 {
    fn from(value: Mvfr0) -> Self {
        value.0
    }
}

impl MemoryMappedRegister for Mvfr0 {
    const ADDRESS: u64 = 0xE000_EF40;
    const NAME: &'static str = "MVFR0";
}

pub(crate) fn read_core_reg(memory: &mut dyn ArmProbe, addr: RegisterId) -> Result<u32, Error> {
    // Write the DCRSR value to select the register we want to read.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(false); // Perform a read.
    dcrsr_val.set_regsel(addr.into()); // The address of the register to read.

    memory.write_word_32(Dcrsr::ADDRESS, dcrsr_val.into())?;

    wait_for_core_register_transfer(memory, Duration::from_millis(100))?;

    let value = memory.read_word_32(Dcrdr::ADDRESS)?;

    Ok(value)
}

pub(crate) fn write_core_reg(
    memory: &mut dyn ArmProbe,
    addr: RegisterId,
    value: u32,
) -> Result<(), Error> {
    memory.write_word_32(Dcrdr::ADDRESS, value)?;

    // write the DCRSR value to select the register we want to write.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(true); // Perform a write.
    dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

    memory.write_word_32(Dcrsr::ADDRESS, dcrsr_val.into())?;

    wait_for_core_register_transfer(memory, Duration::from_millis(100))?;

    Ok(())
}

fn wait_for_core_register_transfer(
    memory: &mut dyn ArmProbe,
    timeout: Duration,
) -> Result<(), ArmError> {
    // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
    // (see C1-292, cortex m0 arm)
    let start = Instant::now();

    while start.elapsed() < timeout {
        let dhcsr_val = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

        if dhcsr_val.s_regrdy() {
            return Ok(());
        }
    }
    Err(ArmError::Timeout)
}
