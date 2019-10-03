use crate::debug_probe::{
    MasterProbe,
    CpuInformation,
    DebugProbeError,
};
use arm_memory::MI;
use super::{
    CoreRegister,
    CoreRegisterAddress,
    Core,
};
use bitfield::bitfield;

use serde::{ Serialize, Deserialize };
use log::debug;

#[derive(Debug,Serialize,Deserialize)]
pub struct CortexDump {
    pub regs:  [u32;16],
    stack_addr: u32,
    stack: Vec<u8>,
}

impl CortexDump {
    pub fn new(stack_addr: u32, stack: Vec<u8>) -> CortexDump {
        CortexDump {
            regs: [0u32;16],
            stack_addr,
            stack,
        }
    }
}

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

impl Dhcsr {
    /// This function sets the bit to enable writes to this register.
    /// 
    /// C1.6.3 Debug Halting Control and Status Register, DHCSR:
    /// Debug key:
    /// Software must write 0xA05F to this field to enable write accesses to bits
    /// [15:0], otherwise the processor ignores the write access.
    pub fn enable_write(&mut self) {
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

impl CoreRegister for Dhcsr {
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

impl CoreRegister for Dcrsr {
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

impl CoreRegister for Dcrdr {
    const ADDRESS: u32 = 0xE000_EDF8;
    const NAME: &'static str = "DCRDR";
}

bitfield!{
    #[derive(Copy, Clone)]
    pub struct BpCtrl(u32);
    impl Debug;
    /// The number of breakpoint comparators. If NUM_CODE is zero, the implementation does not support any comparators
    pub numcode, _: 7, 4;
    /// RAZ on reads, SBO, for writes. If written as zero, the write to the register is ignored.
    pub key, set_key: 1;
    /// Enables the BPU:
    /// 0 BPU is disabled.
    /// 1 BPU is enabled.
    /// This bit is set to 0 on a power-on reset
    pub _, set_enable: 0;
}

impl From<u32> for BpCtrl {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<BpCtrl> for u32 {
    fn from(value: BpCtrl) -> Self {
        value.0
    }
}

impl CoreRegister for BpCtrl {
    const ADDRESS: u32 = 0xE000_2000;
    const NAME: &'static str = "BP_CTRL";
}

bitfield!{
    #[derive(Copy, Clone)]
    pub struct BpCompx(u32);
    impl Debug;
    /// BP_MATCH defines the behavior when the COMP address is matched:
    /// - 00 no breakpoint matching.
    /// - 01 breakpoint on lower halfword, upper is unaffected.
    /// - 10 breakpoint on upper halfword, lower is unaffected.
    /// - 11 breakpoint on both lower and upper halfwords.
    /// - The field is UNKNOWN on reset.
    pub _, set_bp_match: 31,30;
    /// Stores bits [28:2] of the comparison address. The comparison address is
    /// compared with the address from the Code memory region. Bits [31:29] and
    /// [1:0] of the comparison address are zero.
    /// The field is UNKNOWN on power-on reset.
    pub _, set_comp: 28,2;
    /// Enables the comparator:
    /// 0 comparator is disabled.
    /// 1 comparator is enabled.
    /// This bit is set to 0 on a power-on reset.
    pub _, set_enable: 0;
}

impl From<u32> for BpCompx {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<BpCompx> for u32 {
    fn from(value: BpCompx) -> Self {
        value.0
    }
}

impl CoreRegister for BpCompx {
    const ADDRESS: u32 = 0xE000_2008;
    const NAME: &'static str = "BP_CTRL0";
}

pub const R0:  CoreRegisterAddress = CoreRegisterAddress(0b00000);
pub const R1:  CoreRegisterAddress = CoreRegisterAddress(0b00001);
pub const R2:  CoreRegisterAddress = CoreRegisterAddress(0b00010);
pub const R3:  CoreRegisterAddress = CoreRegisterAddress(0b00011);
pub const R4:  CoreRegisterAddress = CoreRegisterAddress(0b00100);
pub const R9:  CoreRegisterAddress = CoreRegisterAddress(0b01001);
pub const PC:  CoreRegisterAddress = CoreRegisterAddress(0b01111);
pub const SP:  CoreRegisterAddress = CoreRegisterAddress(0b01101);
pub const LR:  CoreRegisterAddress = CoreRegisterAddress(0b01110);
pub const MSP: CoreRegisterAddress = CoreRegisterAddress(0b01001);
pub const PSP: CoreRegisterAddress = CoreRegisterAddress(0b01010);

#[derive(Debug, Default, Copy, Clone)]
pub struct M0;


impl M0 {
    fn wait_for_core_register_transfer(&self, mi: &mut impl MI) -> Result<(), DebugProbeError> {
        // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
        // (see C1-292, cortex m0 arm)
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(mi.read32(Dhcsr::ADDRESS)?);

            if dhcsr_val.s_regrdy() {
                return Ok(());
            }
        }
        Err(DebugProbeError::Timeout)
    }
}

impl Core for M0 {
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        // Wait until halted state is active again.
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(mi.read32(Dhcsr::ADDRESS)?);

            if dhcsr_val.s_halt() {
                return Ok(());
            }
        }
        Err(DebugProbeError::Timeout)
    }

    fn read_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError> {
        // Write the DCRSR value to select the register we want to read.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(false); // Perform a read.
        dcrsr_val.set_regsel(addr.into());  // The address of the register to read.

        mi.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(mi)?;

        mi.read32(Dcrdr::ADDRESS).map_err(From::from)
    }

    fn write_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError> {
        let result: Result<(), DebugProbeError> = mi.write32(Dcrdr::ADDRESS, value).map_err(From::from);
        result?;

        self.wait_for_core_register_transfer(mi)?;

        // write the DCRSR value to select the register we want to write.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(true); // Perform a write.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

        mi.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(mi)
    }

    fn halt(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        mi.write32(Dhcsr::ADDRESS, value.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(mi, PC)?;

        // get pc
        Ok(CpuInformation {
            pc: pc_value,
        })
    }

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        let mut value = Dhcsr(0);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.enable_write();

        mi.write32(Dhcsr::ADDRESS, value.into()).map_err(Into::into)
    }

    fn step(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError> {
        let mut value = Dhcsr(0);
        // Leave halted state.
        // Step one instruction.
        value.set_c_step(true);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.set_c_maskints(true);
        value.enable_write();

        mi.write32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted(mi)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(mi, PC)?;

        // get pc
        Ok(CpuInformation {
            pc: pc_value,
        })
    }

    fn get_available_breakpoint_units(&self, mi: &mut MasterProbe) -> Result<u32, DebugProbeError> {
        let result = mi.read32(BpCtrl::ADDRESS)?;

        self.wait_for_core_register_transfer(mi)?;

        Ok(result)
    }

    fn enable_breakpoints(&self, mi: &mut MasterProbe, state: bool) -> Result<(), DebugProbeError> {
        debug!("Enabling breakpoints: {:?}", state);
        let mut value = BpCtrl(0);
        value.set_key(true);
        value.set_enable(state);

        mi.write32(BpCtrl::ADDRESS, value.into())?;

        self.wait_for_core_halted(mi)
    }

    fn set_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError> {
        debug!("Setting breakpoint on address 0x{:08x}", addr);
        let mut value = BpCompx(0);
        value.set_bp_match(0b11);
        value.set_comp((addr >> 2) & 0x00FFFFFF);
        value.set_enable(true);

        mi.write32(BpCompx::ADDRESS, value.into())?;

        self.wait_for_core_halted(mi)
    }

    fn enable_breakpoint(&self, _mi: &mut MasterProbe, _addr: u32) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn disable_breakpoint(&self, _mi: &mut MasterProbe, _addr: u32) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn read_block8(&self, mi: &mut MasterProbe, address: u32, data: &mut [u8]) -> Result<(), DebugProbeError> {
        Ok(mi.read_block8(address, data)?)
    }
}

pub struct FakeM0 {
    dump: CortexDump,
}

impl FakeM0 {
    pub fn new(dump: CortexDump) -> FakeM0 {
        FakeM0 {
            dump
        }
    }
}

impl Core for FakeM0 {
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        unimplemented!();
    }
    
    fn halt(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError> {
        unimplemented!()
    }

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    /// Steps one instruction and then enters halted state again.
    fn step(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError> {
        unimplemented!()
    }

    fn read_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError> {
        let index: u32 = addr.into();

        self.dump.regs.get(index as usize).map(|v| *v).ok_or(DebugProbeError::UnknownError)
    }

    fn write_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn get_available_breakpoint_units(&self, mi: &mut MasterProbe) -> Result<u32, DebugProbeError> {
        unimplemented!()
    }

    fn enable_breakpoints(&self, mi: &mut MasterProbe, state: bool) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn set_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn enable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError> {
        unimplemented!()
    }

    fn disable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError> { 
        unimplemented!()
    }

    fn read_block8(&self, mi: &mut MasterProbe, address: u32, data: &mut [u8]) -> Result<(), DebugProbeError> {
        debug!("Read from dump: addr=0x{:08x}, len={}", address, data.len());

        if (address < self.dump.stack_addr) || (address as usize > (self.dump.stack_addr as usize + self.dump.stack.len())) {
            return Err(DebugProbeError::UnknownError);
        }

        if address as usize + data.len() > (self.dump.stack_addr as usize + self.dump.stack.len()) {
            return Err(DebugProbeError::UnknownError);
        }

        let stack_offset = (address - self.dump.stack_addr) as usize;

        data.copy_from_slice(&self.dump.stack[stack_offset..(stack_offset+data.len())]);

        Ok(())
    }

}