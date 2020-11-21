use crate::core::{
    CoreInformation, CoreInterface, CoreRegister, CoreRegisterAddress, RegisterFile,
};
use crate::error::Error;
use crate::memory::Memory;
use crate::DebugProbeError;

use super::{register, reset_catch_clear, reset_catch_set, CortexState, Dfsr, ARM_REGISTER_FILE};
use crate::{
    core::{Architecture, CoreStatus, HaltReason},
    MemoryInterface,
};
use anyhow::Result;

use bitfield::bitfield;
use std::mem::size_of;
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

bitfield! {
    #[derive(Copy,Clone)]
    pub struct FpCtrl(u32);
    impl Debug;

    pub rev, _: 31, 28;
    num_code_1, _: 14, 12;
    pub num_lit, _: 11, 8;
    num_code_0, _: 7, 4;
    pub _, set_key: 1;
    pub enable, set_enable: 0;
}

impl FpCtrl {
    pub fn num_code(&self) -> u32 {
        (self.num_code_1() << 4) | self.num_code_0()
    }
}

impl CoreRegister for FpCtrl {
    const ADDRESS: u32 = 0xE000_2000;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpCtrl {
    fn from(value: u32) -> Self {
        FpCtrl(value)
    }
}

impl From<FpCtrl> for u32 {
    fn from(value: FpCtrl) -> Self {
        value.0
    }
}

bitfield! {
    #[derive(Copy,Clone)]
    pub struct FpRev1CompX(u32);
    impl Debug;

    pub replace, set_replace: 31, 30;
    pub comp, set_comp: 28, 2;
    pub enable, set_enable: 0;
}

impl CoreRegister for FpRev1CompX {
    const ADDRESS: u32 = 0xE000_2008;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpRev1CompX {
    fn from(value: u32) -> Self {
        FpRev1CompX(value)
    }
}

impl From<FpRev1CompX> for u32 {
    fn from(value: FpRev1CompX) -> Self {
        value.0
    }
}

impl FpRev1CompX {
    /// Get the correct register configuration which enables
    /// a hardware breakpoint at the given address.
    fn breakpoint_configuration(address: u32) -> Self {
        let mut reg = FpRev1CompX::from(0);

        let comp_val = (address & 0x1f_ff_ff_fc) >> 2;

        // the replace value decides if the upper or lower half
        // word is matched for the break point
        let replace_val = if (address & 0x3) == 0 {
            0b01 // lower half word
        } else {
            0b10 // upper half word
        };

        reg.set_replace(replace_val);
        reg.set_comp(comp_val);
        reg.set_enable(true);

        reg
    }
}

bitfield! {
    #[derive(Copy,Clone)]
    pub struct FpRev2CompX(u32);
    impl Debug;

    pub bpaddr, set_bpaddr: 31, 1;
    pub enable, set_enable: 0;
}

impl CoreRegister for FpRev2CompX {
    const ADDRESS: u32 = 0xE000_2008;
    const NAME: &'static str = "FP_CTRL";
}

impl From<u32> for FpRev2CompX {
    fn from(value: u32) -> Self {
        FpRev2CompX(value)
    }
}

impl From<FpRev2CompX> for u32 {
    fn from(value: FpRev2CompX) -> Self {
        value.0
    }
}

impl FpRev2CompX {
    /// Get the correct register configuration which enables
    /// a hardware breakpoint at the given address.
    fn breakpoint_configuration(address: u32) -> Self {
        let mut reg = FpRev2CompX::from(0);

        reg.set_bpaddr(address >> 1);
        reg.set_enable(true);

        reg
    }
}

pub const MSP: CoreRegisterAddress = CoreRegisterAddress(0b000_1001);
pub const PSP: CoreRegisterAddress = CoreRegisterAddress(0b000_1010);

pub struct M4<'probe> {
    memory: Memory<'probe>,

    state: &'probe mut CortexState,
}

impl<'probe> M4<'probe> {
    pub(crate) fn new(
        mut memory: Memory<'probe>,
        state: &'probe mut CortexState,
    ) -> Result<M4<'probe>, Error> {
        if !state.initialized() {
            // determine current state
            let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

            let core_state = if dhcsr.s_sleep() {
                CoreStatus::Sleeping
            } else if dhcsr.s_halt() {
                log::debug!("Core was halted when connecting");

                let dfsr = Dfsr(memory.read_word_32(Dfsr::ADDRESS)?);

                let reason = dfsr.halt_reason();

                CoreStatus::Halted(reason)
            } else {
                CoreStatus::Running
            };

            // Clear DFSR register. The bits in the register are sticky,
            // so we clear them here to ensure that that none are set.
            let dfsr_clear = Dfsr::clear_all();

            memory.write_word_32(Dfsr::ADDRESS, dfsr_clear.into())?;

            state.current_state = core_state;
            state.initialize();
        }

        Ok(Self { memory, state })
    }
}

impl<'probe> CoreInterface for M4<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while start.elapsed() < timeout {
            let dhcsr_val = Dhcsr(self.memory.read_word_32(Dhcsr::ADDRESS)?);
            if dhcsr_val.s_halt() {
                // update halted state
                self.status()?;

                return Ok(());
            }
        }
        Err(Error::Probe(DebugProbeError::Timeout))
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        // Wait until halted state is active again.
        let dhcsr_val = Dhcsr(self.memory.read_word_32(Dhcsr::ADDRESS)?);

        if dhcsr_val.s_halt() {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        let dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::ADDRESS)?);

        if dhcsr.s_sleep() {
            // Check if we assumed the core to be halted
            if self.state.current_state.is_halted() {
                log::warn!("Expected core to be halted, but core is running");
            }

            self.state.current_state = CoreStatus::Sleeping;

            return Ok(CoreStatus::Sleeping);
        }

        // TODO: Handle lockup

        if dhcsr.s_halt() {
            let dfsr = Dfsr(self.memory.read_word_32(Dfsr::ADDRESS)?);

            let reason = dfsr.halt_reason();

            // Clear bits from Dfsr register
            self.memory
                .write_word_32(Dfsr::ADDRESS, Dfsr::clear_all().into())?;

            // If the core was halted before, we cannot read the halt reason from the chip,
            // because we clear it directly after reading.
            if self.state.current_state.is_halted() {
                // There shouldn't be any bits set, otherwise it means
                // that the reason for the halt has changed. No bits set
                // means that we have an unkown HaltReason.
                if reason == HaltReason::Unknown {
                    return Ok(self.state.current_state);
                }

                log::debug!(
                    "Reason for halt has changed, old reason was {:?}, new reason is {:?}",
                    &self.state.current_state,
                    &reason
                );
            }

            self.state.current_state = CoreStatus::Halted(reason);

            return Ok(CoreStatus::Halted(reason));
        }

        // Core is neither halted nor sleeping, so we assume it is running.
        if self.state.current_state.is_halted() {
            log::warn!("Core is running, but we expected it to be halted");
        }

        self.state.current_state = CoreStatus::Running;

        Ok(CoreStatus::Running)
    }

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, Error> {
        self.memory.read_core_reg(address)
    }

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()> {
        self.memory.write_core_reg(address, value)?;

        Ok(())
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted(timeout)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn run(&mut self) -> Result<(), Error> {
        let mut value = Dhcsr(0);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;
        self.memory.flush()?;

        // We assume that the core is running now
        self.state.current_state = CoreStatus::Running;

        Ok(())
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        let mut value = Dhcsr(0);
        // Leave halted state.
        // Step one instruction.
        value.set_c_step(true);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.set_c_maskints(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted(Duration::from_millis(100))?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn reset(&mut self) -> Result<(), Error> {
        // Set THE AIRCR.SYSRESETREQ control bit to 1 to request a reset. (ARM V6 ARM, B1.5.16)
        let mut value = Aircr(0);
        value.vectkey();
        value.set_sysresetreq(true);

        self.memory.write_word_32(Aircr::ADDRESS, value.into())?;

        Ok(())
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        // Set the vc_corereset bit in the DEMCR register.
        // This will halt the core after reset.
        reset_catch_set(self)?;

        self.reset()?;

        self.wait_for_core_halted(timeout)?;

        const XPSR_THUMB: u32 = 1 << 24;
        let xpsr_value = self.read_core_reg(register::XPSR.address)?;
        if xpsr_value & XPSR_THUMB == 0 {
            self.write_core_reg(register::XPSR.address, xpsr_value | XPSR_THUMB)?;
        }

        reset_catch_clear(self)?;

        // try to read the program counter
        let pc_value = self.read_core_reg(register::PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn get_available_breakpoint_units(&mut self) -> Result<u32, Error> {
        let raw_val = self.memory.read_word_32(FpCtrl::ADDRESS)?;

        let reg = FpCtrl::from(raw_val);

        if reg.rev() == 0 || reg.rev() == 1 {
            Ok(reg.num_code())
        } else {
            log::warn!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", reg.rev());
            Err(Error::Probe(DebugProbeError::CommandNotSupportedByProbe))
        }
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        let mut val = FpCtrl::from(0);
        val.set_key(true);
        val.set_enable(state);

        self.memory.write_word_32(FpCtrl::ADDRESS, val.into())?;

        self.state.hw_breakpoints_enabled = true;

        Ok(())
    }

    fn set_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), Error> {
        let raw_val = self.memory.read_word_32(FpCtrl::ADDRESS)?;
        let ctrl_reg = FpCtrl::from(raw_val);

        let val: u32;
        if ctrl_reg.rev() == 0 {
            val = FpRev1CompX::breakpoint_configuration(addr).into();
        } else if ctrl_reg.rev() == 1 {
            val = FpRev2CompX::breakpoint_configuration(addr).into();
        } else {
            log::warn!("This chip uses FPBU revision {}, which is not yet supported. HW breakpoints are not available.", ctrl_reg.rev());
            return Err(Error::Probe(DebugProbeError::CommandNotSupportedByProbe));
        }

        // This is fine as FpRev1CompX and Rev2CompX are just two different
        // interpretations of the same memory region as Rev2 can handle bigger
        // address spaces than Rev1.
        let reg_addr = FpRev1CompX::ADDRESS + (bp_unit_index * size_of::<u32>()) as u32;

        self.memory.write_word_32(reg_addr, val)?;

        Ok(())
    }

    fn registers(&self) -> &'static RegisterFile {
        &ARM_REGISTER_FILE
    }

    fn clear_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let mut val = FpRev1CompX::from(0);
        val.set_enable(false);

        let reg_addr = FpRev1CompX::ADDRESS + (bp_unit_index * size_of::<u32>()) as u32;

        self.memory.write_word_32(reg_addr, val.into())?;

        Ok(())
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.hw_breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }
}

impl<'probe> MemoryInterface for M4<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        self.memory.read_word_32(address)
    }
    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        self.memory.read_word_8(address)
    }
    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        self.memory.read_32(address, data)
    }
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.memory.read_8(address, data)
    }
    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        self.memory.write_word_32(address, data)
    }
    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        self.memory.write_word_8(address, data)
    }
    fn write_32(&mut self, address: u32, data: &[u32]) -> Result<(), Error> {
        self.memory.write_32(address, data)
    }
    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), Error> {
        self.memory.write_8(address, data)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.memory.flush()
    }
}

#[test]
fn breakpoint_register_value() {
    // Check that the register configuration for the FPBU is
    // calculated correctly.
    //
    // See ARMv7 Architecture Reference Manual, Section C1.11.5
    let address: u32 = 0x0800_09A4;

    let reg = FpRev1CompX::breakpoint_configuration(address);
    let reg_val: u32 = reg.into();

    assert_eq!(0x4800_09A5, reg_val);
}
