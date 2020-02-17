//! RISCV Support

use crate::core::Architecture;
use crate::CoreInterface;
use communication_interface::{AccessRegisterCommand, DebugRegister, RiscvCommunicationInterface};

use crate::core::{CoreInformation, RegisterFile};
use crate::CoreRegisterAddress;
use bitfield::bitfield;

#[macro_use]
mod register;

pub mod communication_interface;
pub mod memory_interface;

#[derive(Clone)]
pub struct Riscv32 {
    interface: RiscvCommunicationInterface,
}

impl Riscv32 {
    pub fn new(interface: RiscvCommunicationInterface) -> Self {
        Self { interface }
    }
}

impl CoreInterface for Riscv32 {
    fn wait_for_core_halted(&self) -> Result<(), crate::Error> {
        // poll the
        let num_retries = 10;

        for _ in 0..num_retries {
            let dmstatus: Dmstatus = self.interface.read_dm_register()?;

            log::trace!("{:?}", dmstatus);

            if dmstatus.allhalted() {
                return Ok(());
            }
        }

        todo!("Proper error for core halt timeout")
    }

    fn core_halted(&self) -> Result<bool, crate::Error> {
        unimplemented!()
    }

    fn halt(&self) -> Result<CoreInformation, crate::Error> {
        // write 1 to the haltreq register, which is part
        // of the dmcontrol register

        // read the current dmcontrol register
        let current_dmcontrol: Dmcontrol = self.interface.read_dm_register()?;
        log::debug!("{:?}", current_dmcontrol);

        let mut dmcontrol = Dmcontrol(0);

        dmcontrol.set_haltreq(true);
        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        self.wait_for_core_halted()?;

        // clear the halt request
        let mut dmcontrol = Dmcontrol(0);

        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        Ok(CoreInformation { pc })
    }

    fn run(&self) -> Result<(), crate::Error> {
        // test if core halted?

        // set resume request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);

        self.interface.write_dm_register(dmcontrol)?;

        // check if request has been acknowleged
        let status: Dmstatus = self.interface.read_dm_register()?;

        if !status.allresumeack() {
            todo!("Error, unable to resume")
        };

        // clear resume request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn reset(&self) -> Result<(), crate::Error> {
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
            todo!("Error: Not all harts have reset");
        }

        // acknowledge the reset
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);

        self.interface.write_dm_register(dmcontrol)?;

        Ok(())
    }

    fn reset_and_halt(&self) -> Result<crate::core::CoreInformation, crate::Error> {
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
            todo!("Error: Not all harts have reset and halted");
        }

        // acknowledge the reset, clear the halt request
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);

        self.interface.write_dm_register(dmcontrol)?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        Ok(CoreInformation { pc })
    }

    fn step(&self) -> Result<crate::core::CoreInformation, crate::Error> {
        let mut dcsr = self.read_core_reg(CoreRegisterAddress(0x7b0))?;

        dcsr |= 1 << 2;

        self.write_core_reg(CoreRegisterAddress(0x7b0), dcsr)?;

        self.run()?;

        self.wait_for_core_halted()?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        // clear step request
        let mut dcsr = self.read_core_reg(CoreRegisterAddress(0x7b0))?;

        dcsr &= !(1 << 2);

        self.write_core_reg(CoreRegisterAddress(0x7b0), dcsr)?;

        Ok(CoreInformation { pc })
    }

    fn read_core_reg(&self, address: crate::CoreRegisterAddress) -> Result<u32, crate::Error> {
        // We need to sue the "Access Register Command",
        // which has cmdtype 0

        // write needs to be clear
        // transfer has to be set

        log::debug!("Reading core register at address {:#x}", address.0);

        // if it is a gpr (general purpose register) read using an abstract command,
        // otherwise, use the program buffer
        if address.0 >= 0x1000 && address.0 <= 0x101f {
            let value = self
                .interface
                .abstract_cmd_register_read(address.0 as u32)?;
            Ok(value)
        } else {
            let s0 = self.interface.abstract_cmd_register_read(0x1008)?;

            // csrrs,
            // with rd  = s0
            //      rs1 = x0
            //      csr = address

            let mut csrrs_cmd: u32 = 0b_00000_010_01000_1110011;
            csrrs_cmd |= ((address.0 as u32) & 0xfff) << 20;
            let ebreak_cmd = 0b000000000001_00000_000_00000_1110011;

            // write progbuf0: csrr xxxxxx s0, (address) // lookup correct command
            self.interface.write_dm_register(Progbuf0(csrrs_cmd))?;

            // write progbuf1: ebreak
            self.interface.write_dm_register(Progbuf1(ebreak_cmd))?;

            // command: postexec
            let mut postexec_cmd = AccessRegisterCommand(0);
            postexec_cmd.set_postexec(true);

            self.interface.execute_abstract_command(postexec_cmd.0)?;

            // command: transfer, regno = 0x1008
            let reg_value = self.interface.abstract_cmd_register_read(0x1008)?;

            // restore original value in s0
            self.interface.abstract_cmd_register_write(0x1008, s0)?;

            Ok(reg_value)
        }
    }

    fn write_core_reg(
        &self,
        address: crate::CoreRegisterAddress,
        value: u32,
    ) -> Result<(), crate::Error> {
        if address.0 >= 0x1000 && address.0 <= 0x101f {
            let value = self
                .interface
                .abstract_cmd_register_write(address.0 as u32, value)?;
            Ok(value)
        } else {
            // Backup register s0
            let s0 = self.interface.abstract_cmd_register_read(0x1008)?;

            // csrrw,
            // with rd  = x0
            //      rs1 = s0
            //      csr = address

            // 0x7b041073

            // Write value into s0
            self.interface.abstract_cmd_register_write(0x1008, value)?;

            let mut csrrw_cmd: u32 = 0b_01000_010_00000_1110011;
            csrrw_cmd |= ((address.0 as u32) & 0xfff) << 20;
            let ebreak_cmd = 0b000000000001_00000_000_00000_1110011;

            // write progbuf0: csrr xxxxxx s0, (address) // lookup correct command
            self.interface.write_dm_register(Progbuf0(csrrw_cmd))?;

            // write progbuf1: ebreak
            self.interface.write_dm_register(Progbuf1(ebreak_cmd))?;

            // command: postexec
            let mut postexec_cmd = AccessRegisterCommand(0);
            postexec_cmd.set_postexec(true);

            self.interface.execute_abstract_command(postexec_cmd.0)?;

            // command: transfer, regno = 0x1008
            // restore original value in s0
            self.interface.abstract_cmd_register_write(0x1008, s0)?;

            Ok(())
        }
    }
    fn get_available_breakpoint_units(&self) -> Result<u32, crate::Error> {
        unimplemented!()
    }
    fn enable_breakpoints(&mut self, state: bool) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn set_breakpoint(&self, bp_unit_index: usize, addr: u32) -> Result<(), crate::Error> {
        unimplemented!()
    }

    fn clear_breakpoint(&self, unit_index: usize) -> Result<(), crate::Error> {
        unimplemented!()
    }

    fn registers(&self) -> &'static RegisterFile {
        unimplemented!()
    }
    fn memory(&self) -> crate::Memory {
        self.interface.memory()
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        unimplemented!()
    }

    fn architecture(&self) -> Architecture {
        Architecture::RISCV
    }
}

bitfield! {
    // `dmcontrol` register, located at
    // address 0x10
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

data_register! { pub Data0, 0x04, "data0" }
data_register! { pub Data1, 0x05, "data1" }
data_register! { pub Data2, 0x05, "data2" }
data_register! { pub Data3, 0x05, "data3" }
data_register! { pub Data4, 0x05, "data4" }
data_register! { pub Data5, 0x05, "data5" }
data_register! { pub Data6, 0x05, "data6" }
data_register! { pub Data7, 0x05, "data7" }
data_register! { pub Data8, 0x05, "data8" }
data_register! { pub Data9, 0x05, "data9" }
data_register! { pub Data10, 0x05, "data10" }
data_register! { pub Data11, 0x0f, "data11" }

data_register! { Command, 0x17, "command" }

data_register! { pub Progbuf0, 0x20, "progbuf0" }
data_register! { pub Progbuf1, 0x21, "progbuf1" }
