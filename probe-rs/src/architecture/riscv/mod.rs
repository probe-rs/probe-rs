//! All the interface bits for RISC-V.

#![allow(clippy::inconsistent_digit_grouping)]

use crate::core::Architecture;
use crate::CoreInterface;
use anyhow::{anyhow, Result};
use communication_interface::{
    AbstractCommandErrorKind, DebugRegister, RiscvCommunicationInterface, RiscvError,
};

use crate::core::{CoreInformation, RegisterFile};
use crate::{CoreRegisterAddress, CoreStatus, Error, HaltReason, MemoryInterface};
use bitfield::bitfield;
use register::RISCV_REGISTERS;
use std::time::{Duration, Instant};

#[macro_use]
mod register;
pub(crate) mod assembly;
mod dtm;

pub mod communication_interface;
pub mod sequences;

/// A interface to operate RISC-V cores.
pub struct Riscv32<'probe> {
    interface: &'probe mut RiscvCommunicationInterface,
}

impl<'probe> Riscv32<'probe> {
    /// Create a new RISC-V interface.
    pub fn new(interface: &'probe mut RiscvCommunicationInterface) -> Self {
        Self { interface }
    }

    fn read_csr(&mut self, address: u16) -> Result<u32, RiscvError> {
        // We need to use the "Access Register Command",
        // which has cmdtype 0

        // write needs to be clear
        // transfer has to be set

        log::debug!("Reading CSR {:#x}", address);

        // always try to read register with abstract command, fallback to program buffer,
        // if not supported
        match self.interface.abstract_cmd_register_read(address) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                log::debug!("Could not read core register {:#x} with abstract command, falling back to program buffer", address);
                self.interface.read_csr_progbuf(address)
            }
            other => other,
        }
    }

    fn write_csr(&mut self, address: u16, value: u32) -> Result<(), RiscvError> {
        log::debug!("Writing CSR {:#x}", address);

        match self.interface.abstract_cmd_register_write(address, value) {
            Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::NotSupported)) => {
                log::debug!("Could not write core register {:#x} with abstract command, falling back to program buffer", address);
                self.interface.write_csr_progbuf(address, value)
            }
            other => other,
        }
    }
}

impl<'probe> CoreInterface for Riscv32<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), crate::Error> {
        let start = Instant::now();

        while start.elapsed() < timeout {
            let dmstatus: Dmstatus = self.interface.read_dm_register()?;

            log::trace!("{:?}", dmstatus);

            if dmstatus.allhalted() {
                return Ok(());
            }
        }

        Err(RiscvError::Timeout.into())
    }

    fn core_halted(&mut self) -> Result<bool, crate::Error> {
        let dmstatus: Dmstatus = self.interface.read_dm_register()?;

        Ok(dmstatus.allhalted())
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, crate::Error> {
        // write 1 to the haltreq register, which is part
        // of the dmcontrol register

        // read the current dmcontrol register
        let current_dmcontrol: Dmcontrol = self.interface.read_dm_register()?;
        log::debug!("{:?}", current_dmcontrol);

        let mut dmcontrol = Dmcontrol(0);

        dmcontrol.set_haltreq(true);
        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        self.wait_for_core_halted(timeout)?;

        // clear the halt request
        let mut dmcontrol = Dmcontrol(0);

        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        let pc = self.read_core_reg(register::RISCV_REGISTERS.program_counter.address)?;

        Ok(CoreInformation { pc })
    }

    fn run(&mut self) -> Result<(), crate::Error> {
        // TODO: test if core halted?

        // set resume request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);

        self.interface.write_dm_register(dmcontrol)?;

        // check if request has been acknowleged
        let status: Dmstatus = self.interface.read_dm_register()?;

        if !status.allresumeack() {
            return Err(RiscvError::RequestNotAcknowledged.into());
        };

        // clear resume request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn reset(&mut self) -> Result<(), crate::Error> {
        log::debug!("Resetting core, setting hartreset bit");

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_hartreset(true);

        self.interface.write_dm_register(dmcontrol)?;

        // Read back register to verify reset is supported
        let readback: Dmcontrol = self.interface.read_dm_register()?;

        if readback.hartreset() {
            log::debug!("Clearing hartreset bit");
            // Reset is performed by setting the bit high, and then low again
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_hartreset(false);

            self.interface.write_dm_register(dmcontrol)?;
        } else {
            // Hartreset is not supported, whole core needs to be reset
            //
            // TODO: Cache this
            log::debug!("Hartreset bit not supported, using ndmreset");
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_ndmreset(true);

            self.interface.write_dm_register(dmcontrol)?;

            log::debug!("Clearing ndmreset bit");
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_ndmreset(false);

            self.interface.write_dm_register(dmcontrol)?;
        }

        // check that cores have reset

        let readback: Dmstatus = self.interface.read_dm_register()?;

        if !readback.allhavereset() {
            log::warn!("Dmstatue: {:?}", readback);
            return Err(RiscvError::RequestNotAcknowledged.into());
        }

        // acknowledge the reset
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn reset_and_halt(
        &mut self,
        _timeout: Duration,
    ) -> Result<crate::core::CoreInformation, crate::Error> {
        log::debug!("Resetting core, setting hartreset bit");

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_hartreset(true);
        dmcontrol.set_haltreq(true);

        self.interface.write_dm_register(dmcontrol)?;

        // Read back register to verify reset is supported
        let readback: Dmcontrol = self.interface.read_dm_register()?;

        if readback.hartreset() {
            log::debug!("Clearing hartreset bit");
            // Reset is performed by setting the bit high, and then low again
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_haltreq(true);
            dmcontrol.set_hartreset(false);

            self.interface.write_dm_register(dmcontrol)?;
        } else {
            // Hartreset is not supported, whole core needs to be reset
            //
            // TODO: Cache this
            log::debug!("Hartreset bit not supported, using ndmreset");
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_ndmreset(true);
            dmcontrol.set_haltreq(true);

            self.interface.write_dm_register(dmcontrol)?;

            log::debug!("Clearing ndmreset bit");
            let mut dmcontrol = Dmcontrol(0);
            dmcontrol.set_dmactive(true);
            dmcontrol.set_ndmreset(false);
            dmcontrol.set_haltreq(true);

            self.interface.write_dm_register(dmcontrol)?;
        }

        // check that cores have reset
        let readback: Dmstatus = self.interface.read_dm_register()?;

        if !(readback.allhavereset() && readback.allhalted()) {
            return Err(RiscvError::RequestNotAcknowledged.into());
        }

        // acknowledge the reset, clear the halt request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);

        self.interface.write_dm_register(dmcontrol)?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        Ok(CoreInformation { pc })
    }

    fn step(&mut self) -> Result<crate::core::CoreInformation, crate::Error> {
        let mut dcsr = Dcsr(self.read_core_reg(CoreRegisterAddress(0x7b0))?);

        dcsr.set_step(true);

        self.write_csr(0x7b0, dcsr.0)?;

        self.run()?;

        self.wait_for_core_halted(Duration::from_millis(100))?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        // clear step request
        let mut dcsr = Dcsr(self.read_core_reg(CoreRegisterAddress(0x7b0))?);

        dcsr.set_step(false);

        self.write_csr(0x7b0, dcsr.0)?;

        Ok(CoreInformation { pc })
    }

    fn read_core_reg(&mut self, address: crate::CoreRegisterAddress) -> Result<u32, crate::Error> {
        self.read_csr(address.0).map_err(|e| e.into())
    }

    fn write_core_reg(&mut self, address: crate::CoreRegisterAddress, value: u32) -> Result<()> {
        self.write_csr(address.0, value).map_err(|e| e.into())
    }

    fn get_available_breakpoint_units(&mut self) -> Result<u32, crate::Error> {
        // TODO: This should probably only be done once, when initialising

        log::debug!("Determining number of HW breakpoints supported");

        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tinfo = 0x7a4;

        let mut tselect_index = 0;

        // These steps follow the debug specification 0.13, section 5.1 Enumeration
        loop {
            log::debug!("Trying tselect={}", tselect_index);
            if let Err(e) = self.write_csr(tselect, tselect_index) {
                match e {
                    RiscvError::AbstractCommand(AbstractCommandErrorKind::Exception) => break,
                    other_error => return Err(other_error.into()),
                }
            }

            let readback = self.read_csr(tselect)?;

            if readback != tselect_index {
                break;
            }

            match self.read_csr(tinfo) {
                Ok(tinfo_val) => {
                    if tinfo_val & 0xffff == 1 {
                        // Trigger doesn't exist, break the loop
                        break;
                    } else {
                        log::info!(
                            "Discovered trigger with index {} and type {}",
                            tselect_index,
                            tinfo_val & 0xffff
                        );
                    }
                }
                Err(RiscvError::AbstractCommand(AbstractCommandErrorKind::Exception)) => {
                    // An exception means we have to read tdata1 to discover the type
                    let tdata_val = self.read_csr(tdata1)?;

                    // TODO: Proper handle xlen
                    let xlen = 32;

                    let trigger_type = tdata_val >> (xlen - 4);

                    if trigger_type == 0 {
                        break;
                    }

                    log::info!(
                        "Discovered trigger with index {} and type {}",
                        tselect_index,
                        trigger_type,
                    );
                }
                Err(other) => return Err(other.into()),
            }

            tselect_index += 1;
        }

        log::debug!("Target supports {} breakpoints.", tselect_index);

        Ok(tselect_index)
    }

    fn enable_breakpoints(&mut self, _state: bool) -> Result<(), crate::Error> {
        // seems not needed on RISCV
        Ok(())
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), crate::Error> {
        // select requested trigger
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        log::warn!("Setting breakpoint {}", bp_unit_index);

        self.write_csr(tselect, bp_unit_index as u32)?;

        // verify the trigger has the correct type

        let tdata_value = Mcontrol(self.read_csr(tdata1)?);

        // This should not happen
        let trigger_type = tdata_value.type_();
        if trigger_type != 0b10 {
            return Err(RiscvError::UnexpectedTriggerType(trigger_type).into());
        }

        // Setup the trigger

        let mut instruction_breakpoint = Mcontrol(0);

        // Enter debug mode
        instruction_breakpoint.set_action(1);

        // Match exactly the value in tdata2
        instruction_breakpoint.set_match(0);

        instruction_breakpoint.set_m(true);
        instruction_breakpoint.set_s(true);
        instruction_breakpoint.set_u(true);

        // Trigger when instruction is executed
        instruction_breakpoint.set_execute(true);

        instruction_breakpoint.set_dmode(true);

        // Match address
        instruction_breakpoint.set_select(false);

        self.write_csr(tdata1, instruction_breakpoint.0)?;
        self.write_csr(tdata2, addr)?;

        Ok(())
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), crate::Error> {
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        self.write_csr(tselect, unit_index as u32)?;
        self.write_csr(tdata1, 0)?;
        self.write_csr(tdata2, 0)?;

        Ok(())
    }

    fn registers(&self) -> &'static RegisterFile {
        &RISCV_REGISTERS
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        // No special enable on RISC

        true
    }

    fn architecture(&self) -> Architecture {
        Architecture::Riscv
    }

    fn status(&mut self) -> Result<crate::core::CoreStatus, crate::Error> {
        // TODO: We should use hartsum to determine if any hart is halted
        //       quickly

        let status: Dmstatus = self.interface.read_dm_register()?;

        if status.allhalted() {
            // determine reason for halt
            let dcsr = Dcsr(self.read_core_reg(CoreRegisterAddress::from(0x7b0))?);

            let reason = match dcsr.cause() {
                // An ebreak instruction was hit
                1 => HaltReason::Breakpoint,
                // Trigger module caused halt
                2 => HaltReason::Breakpoint,
                // Debugger requested a halt
                3 => HaltReason::Request,
                // Core halted after single step
                4 => HaltReason::Step,
                // Core halted directly after reset
                5 => HaltReason::Exception,
                // Reserved for future use in specification
                _ => HaltReason::Unknown,
            };

            Ok(CoreStatus::Halted(reason))
        } else if status.allrunning() {
            Ok(CoreStatus::Running)
        } else {
            Err(
                anyhow!("Some cores are running while some are halted, this should not happen.")
                    .into(),
            )
        }
    }

    /// See docs on the [`CoreInterface::get_hw_breakpoints`] trait
    /// NOTE: For riscv, this assumes that only execution breakpoints are used.
    fn get_hw_breakpoints(&mut self) -> Result<Vec<Option<u32>>, Error> {
        let tselect = 0x7a0;
        let tdata1 = 0x7a1;
        let tdata2 = 0x7a2;

        let mut breakpoints = vec![];
        let num_hw_breakpoints = self.get_available_breakpoint_units()? as usize;
        for bp_unit_index in 0..num_hw_breakpoints {
            // Select the trigger.
            self.write_csr(tselect, bp_unit_index as u32)?;

            // Read the trigger "configuration" data.
            let tdata_value = Mcontrol(self.read_csr(tdata1)?);

            log::warn!("Breakpoint {}: {:?}", bp_unit_index, tdata_value);

            // The trigger must be active in at least a single mode
            let trigger_any_mode_active = tdata_value.m() || tdata_value.s() || tdata_value.u();

            let trigger_any_action_enabled =
                tdata_value.execute() || tdata_value.store() || tdata_value.load();

            // Only return if the trigger if it is for an execution debug action in all modes.
            if tdata_value.type_() == 0b10
                && tdata_value.action() == 1
                && tdata_value.match_() == 0
                && trigger_any_mode_active
                && trigger_any_action_enabled
            {
                let breakpoint = self.read_csr(tdata2)?;
                breakpoints.push(Some(breakpoint));
            } else {
                breakpoints.push(None);
            }
        }

        Ok(breakpoints)
    }
}

impl<'probe> MemoryInterface for Riscv32<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        self.interface.read_word_32(address)
    }
    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        self.interface.read_word_8(address)
    }
    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        self.interface.read_32(address, data)
    }
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.interface.read_8(address, data)
    }
    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        self.interface.write_word_32(address, data)
    }
    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        self.interface.write_word_8(address, data)
    }
    fn write_32(&mut self, address: u32, data: &[u32]) -> Result<(), Error> {
        self.interface.write_32(address, data)
    }
    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), Error> {
        self.interface.write_8(address, data)
    }
    fn flush(&mut self) -> Result<(), Error> {
        self.interface.flush()
    }
}

bitfield! {
    /// `dmcontrol` register, located at
    /// address 0x10
    #[derive(Copy, Clone)]
    pub struct Dmcontrol(u32);
    impl Debug;

    _, set_haltreq: 31;
    _, set_resumereq: 30;
    hartreset, set_hartreset: 29;
    _, set_ackhavereset: 28;
    hasel, set_hasel: 26;
    hartsello, set_hartsello: 25, 16;
    hartselhi, set_hartselhi: 15, 6;
    _, set_resethaltreq: 3;
    _, set_clrresethaltreq: 2;
    ndmreset, set_ndmreset: 1;
    dmactive, set_dmactive: 0;
}

impl Dmcontrol {
    /// Currently selected harts
    ///
    /// Combination of the hartselhi and hartsello registers.
    fn hartsel(&self) -> u32 {
        self.hartselhi() << 10 | self.hartsello()
    }

    /// Set the currently selected harts
    ///
    /// This sets the hartselhi and hartsello registers.
    /// This is a 20 bit register, larger values will be truncated.
    fn set_hartsel(&mut self, value: u32) {
        self.set_hartsello(value & 0x3ff);
        self.set_hartselhi((value >> 10) & 0x3ff);
    }
}

impl DebugRegister for Dmcontrol {
    const ADDRESS: u8 = 0x10;
    const NAME: &'static str = "dmcontrol";
}

impl From<Dmcontrol> for u32 {
    fn from(register: Dmcontrol) -> Self {
        register.0
    }
}

impl From<u32> for Dmcontrol {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

bitfield! {
    /// Readonly `dmstatus` register.
    ///
    /// Located at address 0x11
    pub struct Dmstatus(u32);
    impl Debug;

    impebreak, _: 22;
    allhavereset, _: 19;
    anyhavereset, _: 18;
    allresumeack, _: 17;
    anyresumeack, _: 16;
    allnonexistent, _: 15;
    anynonexistent, _: 14;
    allunavail, _: 13;
    anyunavail, _: 12;
    allrunning, _: 11;
    anyrunning, _: 10;
    allhalted, _: 9;
    anyhalted, _: 8;
    authenticated, _: 7;
    authbusy, _: 6;
    hasresethaltreq, _: 5;
    confstrptrvalid, _: 4;
    version, _: 3, 0;
}

impl DebugRegister for Dmstatus {
    const ADDRESS: u8 = 0x11;
    const NAME: &'static str = "dmstatus";
}

impl From<u32> for Dmstatus {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<Dmstatus> for u32 {
    fn from(register: Dmstatus) -> Self {
        register.0
    }
}

bitfield! {
        struct Dcsr(u32);
        impl Debug;

        xdebugver, _: 31, 28;
        ebreakm, set_ebreakm: 15;
        ebreaks, set_ebreaks: 13;
        ebreaku, set_ebreaku: 12;
        stepie, set_stepie: 11;
        stopcount, set_stopcount: 10;
        stoptime, set_stoptime: 9;
        cause, _: 8, 6;
        mprven, set_mprven: 4;
        nmip, _: 3;
        step, set_step: 2;
        prv, set_prv: 1,0;
}

bitfield! {
    /// Abstract Control and Status (see 3.12.6)
    pub struct Abstractcs(u32);
    impl Debug;

    progbufsize, _: 28, 24;
    busy, _: 12;
    cmderr, set_cmderr: 10, 8;
    datacount, _: 3, 0;
}

impl DebugRegister for Abstractcs {
    const ADDRESS: u8 = 0x16;
    const NAME: &'static str = "abstractcs";
}

impl From<Abstractcs> for u32 {
    fn from(register: Abstractcs) -> Self {
        register.0
    }
}

impl From<u32> for Abstractcs {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

bitfield! {
    /// Hart Info (see 3.12.3)
    pub struct Hartinfo(u32);
    impl Debug;

    nscratch, _: 23, 20;
    dataaccess, _: 16;
    datasize, _: 15, 12;
    dataaddr, _: 11, 0;
}

impl DebugRegister for Hartinfo {
    const ADDRESS: u8 = 0x12;
    const NAME: &'static str = "hartinfo";
}

impl From<Hartinfo> for u32 {
    fn from(register: Hartinfo) -> Self {
        register.0
    }
}

impl From<u32> for Hartinfo {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

data_register! { pub Data0, 0x04, "data0" }
data_register! { pub Data1, 0x05, "data1" }
data_register! { pub Data2, 0x06, "data2" }
data_register! { pub Data3, 0x07, "data3" }
data_register! { pub Data4, 0x08, "data4" }
data_register! { pub Data5, 0x09, "data5" }
data_register! { pub Data6, 0x0A, "data6" }
data_register! { pub Data7, 0x0B, "data7" }
data_register! { pub Data8, 0x0C, "data8" }
data_register! { pub Data9, 0x0D, "data9" }
data_register! { pub Data10, 0x0E, "data10" }
data_register! { pub Data11, 0x0f, "data11" }

data_register! { Command, 0x17, "command" }

data_register! { pub Progbuf0, 0x20, "progbuf0" }
data_register! { pub Progbuf1, 0x21, "progbuf1" }
data_register! { pub Progbuf2, 0x22, "progbuf2" }
data_register! { pub Progbuf3, 0x23, "progbuf3" }
data_register! { pub Progbuf4, 0x24, "progbuf4" }
data_register! { pub Progbuf5, 0x25, "progbuf5" }
data_register! { pub Progbuf6, 0x26, "progbuf6" }
data_register! { pub Progbuf7, 0x27, "progbuf7" }
data_register! { pub Progbuf8, 0x28, "progbuf8" }
data_register! { pub Progbuf9, 0x29, "progbuf9" }
data_register! { pub Progbuf10, 0x2A, "progbuf10" }
data_register! { pub Progbuf11, 0x2B, "progbuf11" }
data_register! { pub Progbuf12, 0x2C, "progbuf12" }
data_register! { pub Progbuf13, 0x2D, "progbuf13" }
data_register! { pub Progbuf14, 0x2E, "progbuf14" }
data_register! { pub Progbuf15, 0x2F, "progbuf15" }

bitfield! {
    struct Mcontrol(u32);
    impl Debug;

    type_, set_type: 31, 28;
    dmode, set_dmode: 27;
    maskmax, _: 26, 21;
    hit, set_hit: 20;
    select, set_select: 19;
    timing, set_timing: 18;
    sizelo, set_sizelo: 17, 16;
    action, set_action: 15, 12;
    chain, set_chain: 11;
    match_, set_match: 10, 7;
    m, set_m: 6;
    s, set_s: 4;
    u, set_u: 3;
    execute, set_execute: 2;
    store, set_store: 1;
    load, set_load: 0;
}
