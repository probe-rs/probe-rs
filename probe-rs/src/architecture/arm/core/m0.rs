use super::CortexDump;
use crate::core::{
    BasicRegisterAddresses, Breakpoint, CoreInformation, CoreInterface, CoreRegister,
    CoreRegisterAddress,
};
use crate::error::Error;
use crate::memory::Memory;
use crate::{DebugProbeError, Probe};
use crate::Session;
use bitfield::bitfield;
use log::debug;
use std::mem::size_of;

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
    pub c_maskings, set_c_maskints: 3;
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

impl CoreRegister for Dhcsr {
    const ADDRESS: u32 = 0xE000_EDF0;
    const NAME: &'static str = "DHCSR";
}

bitfield! {
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

bitfield! {
    #[derive(Copy, Clone)]
    pub struct BpCtrl(u32);
    impl Debug;
    /// The number of breakpoint comparators. If NUM_CODE is zero, the implementation does not support any comparators
    pub num_code, _: 7, 4;
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

bitfield! {
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

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Aircr(u32);
    impl Debug;
    pub get_vectkeystat, set_vectkey: 31,16;
    pub endianness, set_endianness: 15;
    pub sysresetreq, set_sysresetreq: 2;
    pub vectclractive, set_vectclractive: 1;
}

impl From<u32> for Aircr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Aircr> for u32 {
    fn from(value: Aircr) -> Self {
        value.0
    }
}

impl Aircr {
    pub fn vectkey(&mut self) {
        self.set_vectkey(0x05FA);
    }

    pub fn vectkeystat(&self) -> bool {
        self.get_vectkeystat() == 0xFA05
    }
}

impl CoreRegister for Aircr {
    const ADDRESS: u32 = 0xE000_ED0C;
    const NAME: &'static str = "AIRCR";
}

bitfield! {
    #[derive(Copy, Clone)]
    pub struct Demcr(u32);
    impl Debug;
    /// Global enable for DWT
    pub dwtena, set_dwtena: 24;
    /// Enable halting debug trap on a HardFault exception
    pub vc_harderr, set_vc_harderr: 10;
    /// Enable Reset Vector Catch
    pub vc_corereset, set_vc_corereset: 0;
}

impl From<u32> for Demcr {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Demcr> for u32 {
    fn from(value: Demcr) -> Self {
        value.0
    }
}

impl CoreRegister for Demcr {
    const ADDRESS: u32 = 0xe000_edfc;
    const NAME: &'static str = "DEMCR";
}

pub const REGISTERS: BasicRegisterAddresses = BasicRegisterAddresses {
    R0: CoreRegisterAddress(0b0_0000),
    R1: CoreRegisterAddress(0b0_0001),
    R2: CoreRegisterAddress(0b0_0010),
    R3: CoreRegisterAddress(0b0_0011),
    R4: CoreRegisterAddress(0b0_0100),
    R5: CoreRegisterAddress(0b0_0101),
    R6: CoreRegisterAddress(0b0_0110),
    R7: CoreRegisterAddress(0b0_0111),
    R8: CoreRegisterAddress(0b0_1000),
    R9: CoreRegisterAddress(0b0_1001),
    PC: CoreRegisterAddress(0b0_1111),
    SP: CoreRegisterAddress(0b0_1101),
    LR: CoreRegisterAddress(0b0_1110),
    XPSR: CoreRegisterAddress(0b1_0000),
};

pub const MSP: CoreRegisterAddress = CoreRegisterAddress(0b01001);
pub const PSP: CoreRegisterAddress = CoreRegisterAddress(0b01010);

#[derive(Clone)]
pub struct M0 {
    memory: Memory,
    session: Session,

    hw_breakpoints_enabled: bool,
    active_breakpoints: Vec<Breakpoint>,
}

impl M0 {
    pub fn new(session: Session, memory: Memory) -> Self {
        Self {
            session,
            memory,
            hw_breakpoints_enabled: false,
            active_breakpoints: vec![],
        }
    }

    fn wait_for_core_register_transfer(&self) -> Result<(), Error> {
        // now we have to poll the dhcsr register, until the dhcsr.s_regrdy bit is set
        // (see C1-292, cortex m0 arm)
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(self.memory.read32(Dhcsr::ADDRESS)?);

            if dhcsr_val.s_regrdy() {
                return Ok(());
            }
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }
}

impl CoreInterface for M0 {
    fn wait_for_core_halted(&self) -> Result<(), Error> {
        // Wait until halted state is active again.
        for _ in 0..100 {
            let dhcsr_val = Dhcsr(self.memory.read32(Dhcsr::ADDRESS)?);

            if dhcsr_val.s_halt() {
                return Ok(());
            }
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }

    fn core_halted(&self) -> Result<bool, Error> {
        // Wait until halted state is active again.
        let dhcsr_val = Dhcsr(self.memory.read32(Dhcsr::ADDRESS)?);

        if dhcsr_val.s_halt() {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn read_core_reg(&self, addr: CoreRegisterAddress) -> Result<u32, Error> {
        // Write the DCRSR value to select the register we want to read.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(false); // Perform a read.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to read.

        self.memory.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer()?;

        self.memory.read32(Dcrdr::ADDRESS).map_err(From::from)
    }

    fn write_core_reg(&self, addr: CoreRegisterAddress, value: u32) -> Result<(), Error> {
        let result: Result<(), Error> = self
            .memory
            .write32(Dcrdr::ADDRESS, value)
            .map_err(From::from);
        result?;

        // write the DCRSR value to select the register we want to write.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(true); // Perform a write.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

        self.memory.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer()
    }

    fn halt(&self) -> Result<CoreInformation, Error> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory.write32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn run(&self) -> Result<(), Error> {
        let mut value = Dhcsr(0);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory
            .write32(Dhcsr::ADDRESS, value.into())
            .map_err(Into::into)
    }

    fn step(&self) -> Result<CoreInformation, Error> {
        let mut value = Dhcsr(0);
        // Leave halted state.
        // Step one instruction.
        value.set_c_step(true);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.set_c_maskints(true);
        value.enable_write();

        self.memory.write32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn reset(&self) -> Result<(), Error> {
        // Set THE AIRCR.SYSRESETREQ control bit to 1 to request a reset. (ARM V6 ARM, B1.5.16)

        let mut value = Aircr(0);
        value.vectkey();
        value.set_sysresetreq(true);

        self.memory.write32(Aircr::ADDRESS, value.into())?;

        Ok(())
    }

    fn reset_and_halt(&self) -> Result<CoreInformation, Error> {
        // Ensure debug mode is enabled
        let dhcsr_val = Dhcsr(self.memory.read32(Dhcsr::ADDRESS)?);
        if !dhcsr_val.c_debugen() {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            self.memory.write32(Dhcsr::ADDRESS, dhcsr.into())?;
        }

        // Set the vc_corereset bit in the DEMCR register.
        // This will halt the core after reset.
        let demcr_val = Demcr(self.memory.read32(Demcr::ADDRESS)?);
        if !demcr_val.vc_corereset() {
            let mut demcr_enabled = demcr_val;
            demcr_enabled.set_vc_corereset(true);
            self.memory.write32(Demcr::ADDRESS, demcr_enabled.into())?;
        }

        self.reset()?;

        self.wait_for_core_halted()?;

        const XPSR_THUMB: u32 = 1 << 24;
        let xpsr_value = self.read_core_reg(REGISTERS.XPSR)?;
        if xpsr_value & XPSR_THUMB == 0 {
            self.write_core_reg(REGISTERS.XPSR, xpsr_value | XPSR_THUMB)?;
        }

        self.memory.write32(Demcr::ADDRESS, demcr_val.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn get_available_breakpoint_units(&self) -> Result<u32, Error> {
        let result = self.memory.read32(BpCtrl::ADDRESS)?;

        let register = BpCtrl::from(result);

        Ok(register.num_code())
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        debug!("Enabling breakpoints: {:?}", state);
        let mut value = BpCtrl(0);
        value.set_key(true);
        value.set_enable(state);

        self.memory.write32(BpCtrl::ADDRESS, value.into())?;

        self.hw_breakpoints_enabled = true;

        Ok(())
    }

    fn set_breakpoint(&self, bp_register_index: usize, addr: u32) -> Result<(), Error> {
        debug!("Setting breakpoint on address 0x{:08x}", addr);
        let mut value = BpCompx(0);
        value.set_bp_match(0b11);
        value.set_comp((addr >> 2) & 0x00FF_FFFF);
        value.set_enable(true);

        let register_addr = BpCompx::ADDRESS + (bp_register_index * size_of::<u32>()) as u32;

        self.memory.write32(register_addr, value.into())?;

        Ok(())
    }

    fn read_block8(&self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        Ok(self.memory.read_block8(address, data)?)
    }

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses {
        &REGISTERS
    }

    fn clear_breakpoint(&self, bp_unit_index: usize) -> Result<(), Error> {
        let register_addr = BpCompx::ADDRESS + (bp_unit_index * size_of::<u32>()) as u32;

        let mut value = BpCompx::from(0);
        value.set_enable(false);

        self.memory.write32(register_addr, value.into())?;

        Ok(())
    }

    fn memory(&self) -> Memory {
        self.memory.clone()
    }
    fn active_breakpoints(&self) -> &Vec<Breakpoint> {
        &self.active_breakpoints
    }
    fn active_breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> {
        &mut self.active_breakpoints
    }
    fn hw_breakpoints_enabled(&self) -> bool {
        self.hw_breakpoints_enabled
    }
}

#[derive(Debug, Clone)]
pub struct FakeM0 {
    dump: CortexDump,
}

impl FakeM0 {
    pub fn new(dump: CortexDump) -> FakeM0 {
        FakeM0 { dump }
    }
}

impl CoreInterface for FakeM0 {
    fn wait_for_core_halted(&self) -> Result<(), Error> {
        unimplemented!();
    }

    fn core_halted(&self) -> Result<bool, Error> {
        unimplemented!();
    }

    fn halt(&self) -> Result<CoreInformation, Error> {
        unimplemented!()
    }

    fn run(&self) -> Result<(), Error> {
        unimplemented!()
    }

    /// Steps one instruction and then enters halted state again.
    fn step(&self) -> Result<CoreInformation, Error> {
        unimplemented!()
    }

    fn reset(&self) -> Result<(), Error> {
        unimplemented!()
    }

    fn reset_and_halt(&self) -> Result<CoreInformation, Error> {
        unimplemented!()
    }

    fn read_core_reg(&self, addr: CoreRegisterAddress) -> Result<u32, Error> {
        let index: u32 = addr.into();

        self.dump
            .regs
            .get(index as usize)
            .copied()
            .ok_or(Error::Probe(DebugProbeError::Unknown))
    }

    fn write_core_reg(&self, _addr: CoreRegisterAddress, _value: u32) -> Result<(), Error> {
        unimplemented!()
    }

    fn get_available_breakpoint_units(&self) -> Result<u32, Error> {
        unimplemented!()
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), Error> {
        unimplemented!()
    }

    fn set_breakpoint(&self, _bp_unit_index: usize, _addr: u32) -> Result<(), Error> {
        unimplemented!()
    }

    fn read_block8(&self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        debug!("Read from dump: addr=0x{:08x}, len={}", address, data.len());

        if (address < self.dump.stack_addr)
            || (address as usize > (self.dump.stack_addr as usize + self.dump.stack.len()))
        {
            return Err(Error::Probe(DebugProbeError::Unknown));
        }

        if address as usize + data.len() > (self.dump.stack_addr as usize + self.dump.stack.len()) {
            return Err(Error::Probe(DebugProbeError::Unknown));
        }

        let stack_offset = (address - self.dump.stack_addr) as usize;

        data.copy_from_slice(&self.dump.stack[stack_offset..(stack_offset + data.len())]);

        Ok(())
    }

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses {
        &REGISTERS
    }

    fn clear_breakpoint(&self, _bp_unit_index: usize) -> Result<(), Error> {
        unimplemented!()
    }

    fn memory(&self) -> Memory {
        unimplemented!()
    }
    fn active_breakpoints(&self) -> &Vec<Breakpoint> {
        unimplemented!()
    }
    fn active_breakpoints_mut(&mut self) -> &mut Vec<Breakpoint> {
        unimplemented!()
    }
    fn hw_breakpoints_enabled(&self) -> bool {
        unimplemented!()
    }
}
