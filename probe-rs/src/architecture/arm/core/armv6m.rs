use super::{Dfsr, State, ARM_REGISTER_FILE};

use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::core::{RegisterDescription, RegisterFile, RegisterKind};
use crate::error::Error;
use crate::memory::Memory;
use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreRegisterAddress, CoreStatus,
    DebugProbeError, HaltReason, MemoryInterface,
};
use anyhow::Result;
use bitfield::bitfield;
use std::sync::Arc;
use std::{
    mem::size_of,
    time::{Duration, Instant},
};

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
    /// \[15:0\], otherwise the processor ignores the write access.
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
    pub bp_match, set_bp_match: 31,30;
    /// Stores bits [28:2] of the comparison address. The comparison address is
    /// compared with the address from the Code memory region. Bits [31:29] and
    /// [1:0] of the comparison address are zero.
    /// The field is UNKNOWN on power-on reset.
    pub comp, set_comp: 28,2;
    /// Enables the comparator:
    /// 0 comparator is disabled.
    /// 1 comparator is enabled.
    /// This bit is set to 0 on a power-on reset.
    pub enable, set_enable: 0;
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

impl BpCompx {
    /// Get the correct comparator value stored at the given address
    /// This will adjust the `BpCompx.comp() result based on the `BpCompx.bp_match()` specification
    /// NOTE: Does not support a `bp_match value of '11'
    fn get_breakpoint_comparator(register_value: u32) -> Result<u32, Error> {
        let bp_val = BpCompx::from(register_value);
        if bp_val.bp_match() == 0b01 {
            Ok(bp_val.comp() << 2)
        } else if bp_val.bp_match() == 0b10 {
            Ok((bp_val.comp() << 2) | 0x2)
        } else {
            return Err(Error::ArchitectureSpecific(Box::new(DebugProbeError::Other(anyhow::anyhow!("Unsupported breakpoint comparator value {:#08x} for HW breakpoint. Breakpoint must be on half-word boundaries", bp_val.0)))));
        }
    }
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
    /// Global enable for DWT.
    /// Enables:
    /// - Data Watchpoint and Trace (DWT)
    /// - Instrumentation Trace Macrocell (ITM)
    /// - Embedded Trace Macrocell (ETM)
    /// - Trace Port Interface Unit (TPIU).
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

/*
const REGISTERS: RegisterFile = RegisterFile {
    registers:
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
*/

pub const MSP: CoreRegisterAddress = CoreRegisterAddress(0b01001);
pub const PSP: CoreRegisterAddress = CoreRegisterAddress(0b01010);

const PC: RegisterDescription = RegisterDescription {
    name: "PC",
    _kind: RegisterKind::PC,
    address: CoreRegisterAddress(0b0_1111),
};

const XPSR: RegisterDescription = RegisterDescription {
    name: "XPSR",
    _kind: RegisterKind::General,
    address: CoreRegisterAddress(0b1_0000),
};

pub struct Armv6m<'probe> {
    memory: Memory<'probe>,

    state: &'probe mut State,

    sequence: Arc<dyn ArmDebugSequence>,
}

impl<'probe> Armv6m<'probe> {
    pub(crate) fn new(
        mut memory: Memory<'probe>,
        state: &'probe mut State,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        if !state.initialized() {
            // determine current state
            let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

            let core_state = if dhcsr.s_sleep() {
                CoreStatus::Sleeping
            } else if dhcsr.s_halt() {
                let dfsr = Dfsr(memory.read_word_32(Dfsr::ADDRESS)?);

                let reason = dfsr.halt_reason();

                log::debug!("Core was halted when connecting, reason: {:?}", reason);

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

        Ok(Self {
            memory,
            state,
            sequence,
        })
    }
}

impl<'probe> CoreInterface for Armv6m<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        // Wait until halted state is active again.
        let start = Instant::now();

        while start.elapsed() < timeout {
            let dhcsr_val = Dhcsr(self.memory.read_word_32(Dhcsr::ADDRESS)?);

            if dhcsr_val.s_halt() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(1));
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

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        // TODO: Generic halt support

        let mut value = Dhcsr(0);
        value.set_c_halt(true);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;

        self.wait_for_core_halted(timeout)?;

        // Update core status
        let _ = self.status()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn run(&mut self) -> Result<(), Error> {
        // Before we run, we always perform a single instruction step, to account for possible breakpoints that might get us stuck on the current instruction.
        self.step()?;

        let mut value = Dhcsr(0);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;
        self.memory.flush()?;

        // We assume that the core is running now.
        self.state.current_state = CoreStatus::Running;

        Ok(())
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        // First check if we stopped on a breakpoint, because this requires special handling before we can continue.
        let was_breakpoint =
            if self.state.current_state == CoreStatus::Halted(HaltReason::Breakpoint) {
                log::debug!("Core was halted on breakpoint, disabling breakpoints");
                self.enable_breakpoints(false)?;
                true
            } else {
                false
            };

        let mut value = Dhcsr(0);
        // Leave halted state.
        // Step one instruction.
        value.set_c_step(true);
        value.set_c_halt(false);
        value.set_c_debugen(true);
        value.set_c_maskints(true);
        value.enable_write();

        self.memory.write_word_32(Dhcsr::ADDRESS, value.into())?;
        self.memory.flush()?;
        self.wait_for_core_halted(Duration::from_millis(100))?;

        // Re-enable breakpoints before we continue.
        if was_breakpoint {
            self.enable_breakpoints(true)?;
        }
        // try to read the program counter
        let pc_value = self.read_core_reg(PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.sequence.reset_system(&mut self.memory)
    }

    fn reset_and_halt(&mut self, _timeout: Duration) -> Result<CoreInformation, Error> {
        self.sequence.reset_and_halt(&mut self.memory)?;

        // Update core status
        let _ = self.status()?;

        // try to read the program counter
        let pc_value = self.read_core_reg(PC.address)?;

        // get pc
        Ok(CoreInformation { pc: pc_value })
    }

    fn get_available_breakpoint_units(&mut self) -> Result<u32, Error> {
        let result = self.memory.read_word_32(BpCtrl::ADDRESS)?;

        let register = BpCtrl::from(result);

        Ok(register.num_code())
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        log::debug!("Enabling breakpoints: {:?}", state);
        let mut value = BpCtrl(0);
        value.set_key(true);
        value.set_enable(state);

        self.memory.write_word_32(BpCtrl::ADDRESS, value.into())?;
        self.memory.flush()?;

        self.state.hw_breakpoints_enabled = state;

        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_register_index: usize, addr: u32) -> Result<(), Error> {
        log::debug!("Setting breakpoint on address 0x{:08x}", addr);

        // The highest 3 bits of the address have to be zero, otherwise the breakpoint cannot
        // be set at the address.
        if addr >= 0x2000_0000 {
            return Err(Error::ArchitectureSpecific(Box::new(DebugProbeError::Other(anyhow::anyhow!("Unsupported address {:#08x} for HW breakpoint. Breakpoint must be at address < 0x2000_0000.", addr)))));
        }

        let mut value = BpCompx(0);
        if addr % 4 < 2 {
            // match lower halfword
            value.set_bp_match(0b01);
        } else {
            // match higher halfword
            value.set_bp_match(0b10);
        }
        value.set_comp((addr >> 2) & 0x07FF_FFFF);
        value.set_enable(true);

        let register_addr = BpCompx::ADDRESS + (bp_register_index * size_of::<u32>()) as u32;

        self.memory.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    fn registers(&self) -> &'static RegisterFile {
        &ARM_REGISTER_FILE
    }

    fn clear_hw_breakpoint(&mut self, bp_unit_index: usize) -> Result<(), Error> {
        let register_addr = BpCompx::ADDRESS + (bp_unit_index * size_of::<u32>()) as u32;

        let mut value = BpCompx::from(0);
        value.set_enable(false);

        self.memory.write_word_32(register_addr, value.into())?;

        Ok(())
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.state.hw_breakpoints_enabled
    }

    fn architecture(&self) -> Architecture {
        Architecture::Arm
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, Error> {
        let dhcsr = Dhcsr(self.memory.read_word_32(Dhcsr::ADDRESS)?);

        if dhcsr.s_lockup() {
            log::warn!("The core is in locked up status as a result of an unrecoverable exception");

            self.state.current_state = CoreStatus::LockedUp;

            return Ok(CoreStatus::LockedUp);
        }

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
                    log::debug!("Cached halt reason: {:?}", self.state.current_state);
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

    /// See docs on the [`CoreInterface::get_hw_breakpoints`] trait
    fn get_hw_breakpoints(&mut self) -> Result<Vec<Option<u32>>, Error> {
        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.get_available_breakpoint_units()? as usize;
        for bp_unit_index in 0..num_hw_breakpoints {
            let reg_addr = BpCompx::ADDRESS + (bp_unit_index * size_of::<u32>()) as u32;
            // The raw breakpoint address as read from memory
            let register_value = self.memory.read_word_32(reg_addr)?;
            if BpCompx::from(register_value).enable() {
                let breakpoint = BpCompx::get_breakpoint_comparator(register_value)?;
                breakpoints.push(Some(breakpoint));
            } else {
                breakpoints.push(None);
            }
        }
        Ok(breakpoints)
    }
}

impl<'probe> MemoryInterface for Armv6m<'probe> {
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
