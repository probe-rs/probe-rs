use crate::debug_probe::{
    MasterProbe,
    CpuInformation,
    DebugProbeError,
};
use memory::MI;
use super::{
    TargetRegister,
    CoreRegisterAddress,
    Target,
};
use bitfield::bitfield;

bitfield!{
    #[derive(Copy, Clone)]
    pub struct Dhcsr(u32);
    impl Debug;
    pub s_reset_st, _: 25;
    pub s_retire_st, _: 24;
    pub s_lockup, _: 19;
    pub s_sleep, _: 18;
    pub s_halt, _: 17;
    pub s_regrdy, _: 16;
    pub _, set_c_maskints: 3;
    pub _, set_c_step: 2;
    pub _, set_c_halt: 1;
    pub _, set_c_debugen: 0;
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

impl TargetRegister for Dhcsr {
    const ADDRESS: u32 = 0xE000_EDF0;
    const NAME: &'static str = "DHCSR";
}

bitfield!{
    #[derive(Copy, Clone)]
    pub struct Dcrsr(u32);
    impl Debug;
    pub _, set_regwnr: 16;
    pub _, set_regsel: 4,0;
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

impl TargetRegister for Dcrsr {
    const ADDRESS: u32 = 0xE000_EDF4;
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

impl TargetRegister for Dcrdr {
    const ADDRESS: u32 = 0xE000_EDF8;
    const NAME: &'static str = "DCRDR";
}

pub const PC: CoreRegisterAddress = CoreRegisterAddress(0b01111);

fn wait_for_core_halted(mi: &mut impl MI) -> Result<(), DebugProbeError> {
    // Wait until halted state is active again.
    for _ in 0..100 {
        let dhcsr_val = Dhcsr(mi.read(Dhcsr::ADDRESS)?);

        if dhcsr_val.s_halt() {
            return Ok(());
        }
    }
    Err(DebugProbeError::UnknownError)
}

fn wait_for_core_register_transfer(mi: &mut impl MI) -> Result<(), DebugProbeError> {
    // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
    // (see C1-292, cortex m0 arm)
    for _ in 0..100 {
        let dhcsr_val = Dhcsr(mi.read(Dhcsr::ADDRESS)?);

        if dhcsr_val.s_regrdy() {
            return Ok(());
        }
    }
    Err(DebugProbeError::UnknownError)
}

fn read_core_reg (mi: &mut MasterProbe, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError> {
    // Write the DCRSR value to select the register we want to read.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(false); // Perform a read.
    dcrsr_val.set_regsel(addr.into());  // The address of the register to read.

    mi.write::<u32>(Dcrsr::ADDRESS, dcrsr_val.into())?;

    wait_for_core_register_transfer(mi)?;

    mi.read(Dcrdr::ADDRESS).map_err(From::from)
}

fn write_core_reg(mi: &mut MasterProbe, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError> {
    // write the DCRSR value to select the register we want to write.
    let mut dcrsr_val = Dcrsr(0);
    dcrsr_val.set_regwnr(true); // Perform a write.
    dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

    mi.write::<u32>(Dcrsr::ADDRESS, dcrsr_val.into())?;

    wait_for_core_register_transfer(mi)?;

    let result: Result<(), DebugProbeError> = mi.write::<u32>(Dcrdr::ADDRESS, value).map_err(From::from);
    result?;

    wait_for_core_register_transfer(mi)
}

fn halt(mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError> {
    // TODO: Generic halt support

    let mut value = Dhcsr(0);
    value.set_c_halt(true);
    value.set_c_debugen(true);
    value.0 |= (0xa05f << 16);

    let result: Result<(), DebugProbeError> = mi.write::<u32>(Dhcsr::ADDRESS, value.into()).map_err(Into::into);
    result?;

    // try to read the program counter
    let pc_value = read_core_reg(mi, PC)?;

    // get pc
    Ok(CpuInformation {
        pc: pc_value,
    })
}

fn run(mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
    let mut value = Dhcsr(0);
    value.set_c_halt(false);
    value.set_c_debugen(false);
    value.0 |= (0xa05f << 16);

    mi.write::<u32>(Dhcsr::ADDRESS, value.into()).map_err(Into::into)
}

fn step(mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
    let mut value = Dhcsr(0);
    // Leave halted state.
    // Step one instruction.
    value.set_c_step(true);
    value.set_c_halt(false);
    value.set_c_debugen(false);
    value.set_c_maskints(true);

    mi.write::<u32>(Dhcsr::ADDRESS, value.into())?;

    wait_for_core_halted(mi)
}

pub const CORTEX_M0: Target = Target {
    halt,
    run,
    step,
    read_core_reg,
    write_core_reg,
};