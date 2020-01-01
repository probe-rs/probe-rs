use crate::coresight::memory::MI;
use crate::probe::{DebugProbeError, MasterProbe};
use crate::target::{
    BasicRegisterAddresses, Core, CoreInformation, CoreRegister, CoreRegisterAddress,
};
use bitfield::bitfield;

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
    pub c_snapstall, set_c_snapstall: 5;
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
    pub struct Aircr(u32);
    impl Debug;
    pub get_vectkeystat, set_vectkey: 31,16;
    pub endianness, set_endianness: 15;
    pub prigroup, set_prigroup: 10,8;
    pub sysresetreq, set_sysresetreq: 2;
    pub vectclractive, set_vectclractive: 1;
    pub vectreset, set_vectreset: 0;
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
    /// Global enable for DWT and ITM features
    pub trcena, set_trcena: 24;
    /// DebugMonitor semaphore bit
    pub mon_req, set_mon_req: 19;
    /// Step the processor?
    pub mon_step, set_mon_step: 18;
    /// Sets or clears the pending state of the DebugMonitor exception
    pub mon_pend, set_mon_pend: 17;
    /// Enable the DebugMonitor exception
    pub mon_en, set_mon_en: 16;
    /// Enable halting debug trap on a HardFault exception
    pub vc_harderr, set_vc_harderr: 10;
    /// Enable halting debug trap on a fault occurring during exception entry
    /// or exception return
    pub vc_interr, set_vc_interr: 9;
    /// Enable halting debug trap on a BusFault exception
    pub vc_buserr, set_vc_buserr: 8;
    /// Enable halting debug trap on a UsageFault exception caused by a state
    /// information error, for example an Undefined Instruction exception
    pub vc_staterr, set_vc_staterr: 7;
    /// Enable halting debug trap on a UsageFault exception caused by a
    /// checking error, for example an alignment check error
    pub vc_chkerr, set_vc_chkerr: 6;
    /// Enable halting debug trap on a UsageFault caused by an access to a
    /// Coprocessor
    pub vc_nocperr, set_vc_nocperr: 5;
    /// Enable halting debug trap on a MemManage exception.
    pub vc_mmerr, set_vc_mmerr: 4;
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
    R0: CoreRegisterAddress(0b000_0000),
    R1: CoreRegisterAddress(0b000_0001),
    R2: CoreRegisterAddress(0b000_0010),
    R3: CoreRegisterAddress(0b000_0011),
    R4: CoreRegisterAddress(0b000_0100),
    R9: CoreRegisterAddress(0b000_1001),
    PC: CoreRegisterAddress(0b000_1111),
    SP: CoreRegisterAddress(0b000_1101),
    LR: CoreRegisterAddress(0b000_1110),
    XPSR: CoreRegisterAddress(0b001_0000),
};

pub const MSP: CoreRegisterAddress = CoreRegisterAddress(0b000_1001);
pub const PSP: CoreRegisterAddress = CoreRegisterAddress(0b000_1010);

#[derive(Debug, Default, Copy, Clone)]
pub struct M4;

impl M4 {
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

impl Core for M4 {
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

    fn read_core_reg(
        &self,
        mi: &mut MasterProbe,
        addr: CoreRegisterAddress,
    ) -> Result<u32, DebugProbeError> {
        // Write the DCRSR value to select the register we want to read.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(false); // Perform a read.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to read.

        mi.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(mi)?;

        mi.read32(Dcrdr::ADDRESS).map_err(From::from)
    }

    fn write_core_reg(
        &self,
        mi: &mut MasterProbe,
        addr: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        let result: Result<(), DebugProbeError> =
            mi.write32(Dcrdr::ADDRESS, value).map_err(From::from);
        result?;

        // write the DCRSR value to select the register we want to write.
        let mut dcrsr_val = Dcrsr(0);
        dcrsr_val.set_regwnr(true); // Perform a write.
        dcrsr_val.set_regsel(addr.into()); // The address of the register to write.

        mi.write32(Dcrsr::ADDRESS, dcrsr_val.into())?;

        self.wait_for_core_register_transfer(mi)
    }

    fn halt(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        mi.write32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted(mi)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(mi, REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        let mut value = Dhcsr(0);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.enable_write();

        mi.write32(Dhcsr::ADDRESS, value.into()).map_err(Into::into)
    }

    fn step(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError> {
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
        let pc_value = self.read_core_reg(mi, REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn reset(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError> {
        // Set THE AIRCR.SYSRESETREQ control bit to 1 to request a reset. (ARM V6 ARM, B1.5.16)
        let mut value = Aircr(0);
        value.vectkey();
        value.set_sysresetreq(true);

        mi.write32(Aircr::ADDRESS, value.into())?;

        Ok(())
    }

    fn reset_and_halt(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError> {
        // Ensure debug mode is enabled
        let dhcsr_val = Dhcsr(mi.read32(Dhcsr::ADDRESS)?);
        if !dhcsr_val.c_debugen() {
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();
            mi.write32(Dhcsr::ADDRESS, dhcsr.into())?;
        }

        // Set the vc_corereset bit in the DEMCR register.
        // This will halt the core after reset.
        let demcr_val = Demcr(mi.read32(Demcr::ADDRESS)?);
        if !demcr_val.vc_corereset() {
            let mut demcr_enabled = demcr_val;
            demcr_enabled.set_vc_corereset(true);
            mi.write32(Demcr::ADDRESS, demcr_enabled.into())?;
        }

        self.reset(mi)?;

        self.wait_for_core_halted(mi)?;

        const XPSR_THUMB: u32 = 1 << 24;
        let xpsr_value = self.read_core_reg(mi, REGISTERS.XPSR)?;
        if xpsr_value & XPSR_THUMB == 0 {
            self.write_core_reg(mi, REGISTERS.XPSR, xpsr_value | XPSR_THUMB)?;
        }

        mi.write32(Demcr::ADDRESS, demcr_val.into())?;

        // try to read the program counter
        let pc_value = self.read_core_reg(mi, REGISTERS.PC)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn get_available_breakpoint_units(
        &self,
        _mi: &mut MasterProbe,
    ) -> Result<u32, DebugProbeError> {
        unimplemented!();
    }

    fn enable_breakpoints(
        &self,
        _mi: &mut MasterProbe,
        _state: bool,
    ) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn set_breakpoint(
        &self,
        _mi: &mut MasterProbe,
        _bp_unit_index: usize,
        _addr: u32,
    ) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn read_block8(
        &self,
        mi: &mut MasterProbe,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), DebugProbeError> {
        Ok(mi.read_block8(address, data)?)
    }

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses {
        &REGISTERS
    }

    fn clear_breakpoint(
        &self,
        _mi: &mut MasterProbe,
        _bp_unit_index: usize,
    ) -> Result<(), DebugProbeError> {
        unimplemented!()
    }
}
