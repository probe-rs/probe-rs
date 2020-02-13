//! Debug Module Communication
//!
//! This module implements communication with a
//! Debug Module, as described in the RISCV debug
//! specification v0.13.2 .

use super::{Dmcontrol, Dmstatus};
use crate::architecture::riscv::Abstractcs;
use crate::architecture::riscv::Data0;
use crate::architecture::riscv::Data1;
use crate::architecture::riscv::Progbuf0;
use crate::architecture::riscv::Progbuf1;
use crate::DebugProbeError;
use crate::{Error, Memory, MemoryInterface, Probe};

use std::cell::RefCell;
use std::rc::Rc;

use std::convert::TryInto;

use bitfield::bitfield;

#[derive(Clone)]
pub struct RiscvCommunicationInterface {
    inner: Rc<RefCell<InnerRiscvCommunicationInterface>>,
}

impl RiscvCommunicationInterface {
    pub fn new(probe: Probe) -> Self {
        Self {
            inner: Rc::new(RefCell::new(
                InnerRiscvCommunicationInterface::build(probe).unwrap(),
            )),
        }
    }

    pub(super) fn read_dm_register<R: DebugRegister>(&self) -> Result<R, DebugProbeError> {
        self.inner.borrow_mut().read_dm_register()
    }

    pub(super) fn write_dm_register(
        &self,
        register: impl DebugRegister,
    ) -> Result<(), DebugProbeError> {
        self.inner.borrow_mut().write_dm_register(register)
    }

    /// Read the IDCODE register
    pub fn read_idcode(&self) -> Result<u32, DebugProbeError> {
        self.inner.borrow_mut().read_idcode()
    }

    pub fn close(self) -> Probe {
        match Rc::try_unwrap(self.inner) {
            Ok(inner) => inner.into_inner().probe,
            Err(_) => panic!("Failed to unwrap RiscvCommunicationInterface"),
        }
    }

    pub fn memory(&self) -> Memory {
        Memory::new(self.clone())
    }
}

impl MemoryInterface for RiscvCommunicationInterface {
    fn read32(&mut self, address: u32) -> Result<u32, crate::Error> {
        self.inner.borrow_mut().read32(address)
    }
    fn read8(&mut self, address: u32) -> Result<u8, crate::Error> {
        self.inner.borrow_mut().read8(address)
    }
    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        self.inner.borrow_mut().read_block32(address, data)
    }
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        self.inner.borrow_mut().read_block8(address, data)
    }
    fn write32(&mut self, addr: u32, data: u32) -> Result<(), crate::Error> {
        self.inner.borrow_mut().write32(addr, data)
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), crate::Error> {
        self.inner.borrow_mut().write8(addr, data)
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), crate::Error> {
        self.inner.borrow_mut().write_block32(addr, data)
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), crate::Error> {
        self.inner.borrow_mut().write_block8(addr, data)
    }
}

struct InnerRiscvCommunicationInterface {
    probe: Probe,
    abits: u32,
}

impl InnerRiscvCommunicationInterface {
    pub fn build(mut probe: Probe) -> Result<Self, DebugProbeError> {
        // We need a jtag interface

        log::debug!("Building RISCV interface");

        let jtag_interface = probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dtmcs_raw = jtag_interface.read_register(0x10, 32)?;

        let dtmcs = Dtmcs(u32::from_le_bytes((&dtmcs_raw[..]).try_into().unwrap()));

        log::debug!("Dtmcs: {:?}", dtmcs);

        let abits = dtmcs.abits();

        let mut interface = InnerRiscvCommunicationInterface { probe, abits };

        // read the  version of the debug module
        let status: Dmstatus = interface.read_dm_register()?;

        assert!(
            status.version() == 2,
            "Only Debug Module version 0.13 is supported!"
        );

        log::debug!("dmstatus: {:?}", status);

        // enable the debug module
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);

        interface.write_dm_register(control)?;

        Ok(interface)
    }

    /// Read the `dtmcs` register
    fn read_dtmcs(&self) -> u32 {
        todo!();
    }

    fn dmi_hard_reset(&self) -> () {}

    fn dmi_reset(&self) -> () {}

    fn version(&self) -> () {}

    fn idle_cycles(&self) -> () {}

    fn read_idcode(&mut self) -> Result<u32, DebugProbeError> {
        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let value = jtag_interface.read_register(0x1, 32)?;

        Ok(u32::from_le_bytes((&value[..]).try_into().unwrap()))
    }

    pub fn read_dm_register<R: DebugRegister>(&mut self) -> Result<R, DebugProbeError> {
        log::debug!("Reading DM register '{}' at {:#010x}", R::NAME, R::ADDRESS);

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dm_reg = dm_read_reg(R::ADDRESS);
        log::debug!("Sending write command (u64): {:#018x?}", dm_reg);

        let bytes = dm_reg.to_le_bytes();

        log::debug!("Sending write command (hex): {:x?}", bytes);

        // Send read command
        jtag_interface.write_register(0x11, &bytes[..6], 41)?;

        // Read back response
        let response = jtag_interface.read_register(0x11, 41)?;

        let lower_value = u32::from_le_bytes((&response[0..4]).try_into().unwrap());
        let higher_value = u16::from_le_bytes((&response[4..6]).try_into().unwrap());

        let complete_value = ((higher_value as u64) << 32) | (lower_value as u64);

        // Verify that the transfer was ok
        assert!((complete_value & 0x3) == 0, "Last transfer was not ok...");

        let response_value = ((complete_value >> 2) & 0xffff_ffff) as u32;

        log::debug!("Address: {:#010x}", (complete_value >> 34) & 0x3f);

        log::debug!(
            "Read DM register '{}' at {:#010x} = {:#010x}",
            R::NAME,
            R::ADDRESS,
            response_value
        );

        Ok(response_value.into())
    }

    pub fn write_dm_register<R: DebugRegister>(
        &mut self,
        register: R,
    ) -> Result<(), DebugProbeError> {
        // write write command to dmi register

        let data = register.into();

        log::debug!(
            "Write DM register '{}' at {:#010x} = {:#010x}",
            R::NAME,
            R::ADDRESS,
            data
        );

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dm_reg = dm_write_reg(R::ADDRESS, data);

        let bytes = dm_reg.to_le_bytes();

        jtag_interface.write_register(0x11, &bytes[..6], 41)?;

        Ok(())
    }

    /// Perfrom memory read from a single location using the program buffer.
    /// Only reads up to a width of 32 bits are currently supported.
    /// For widths smaller than u32, the higher bits have to be discarded manually.
    fn perform_memory_read(&mut self, address: u32, width: u8) -> Result<u32, Error> {
        // assemble
        //  lb s1, 0(s0)

        //let o = 0; // offset = 0
        //let b = 9; // base register -> s0
        //let w = 0; // width
        //let d = 9; // dest register -> s0
        //let l = 0b11;

        //let lw_command = bitpack!("oooooooooooobbbbbwwwddddd_lllllll");
        let mut lw_command: u32 = 0b000000000000_01000_000_01000_0000011;

        // verify the width is supported
        // 0 ==  8 bit
        // 1 == 16 bit
        // 2 == 32 bit

        assert!(width < 3, "Width larger than 3 not supported yet");

        lw_command |= (width as u32) << 12;

        let ebreak_cmd = 0b000000000001_00000_000_00000_1110011;

        self.write_dm_register(Progbuf0(lw_command))?;
        self.write_dm_register(Progbuf1(ebreak_cmd))?;

        self.write_dm_register(Data0(address))?;

        // Write s0, then execute program buffer
        let mut command = AccessRegisterCommand(0);
        command.set_cmd_type(0);
        command.set_transfer(true);
        command.set_write(true);

        // registers are 32 bit, so we have size 2 here
        command.set_aarsize(2);
        command.set_postexec(true);

        // register s0, ie. 0x1008
        command.set_regno(0x1008);

        self.write_dm_register(command)?;

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            todo!("Error code for command execution ({:?})", status);
        }

        // Execute program buffer (how?)

        // Read back s0
        let mut read_command = AccessRegisterCommand(0);
        read_command.set_cmd_type(0);
        read_command.set_transfer(true);
        read_command.set_regno(0x1008);
        read_command.set_aarsize(2);

        self.write_dm_register(read_command)?;

        // Ensure commands run without error

        let status: Abstractcs = self.read_dm_register()?;

        if status.cmderr() != 0 {
            todo!("Error code for command execution ({:?})", status);
        }

        let value: Data0 = self.read_dm_register()?;

        Ok(u32::from(value))
    }
}

impl MemoryInterface for InnerRiscvCommunicationInterface {
    fn read32(&mut self, address: u32) -> Result<u32, crate::Error> {
        self.perform_memory_read(address, 2)
    }
    fn read8(&mut self, address: u32) -> Result<u8, crate::Error> {
        let value = self.perform_memory_read(address, 0)?;

        Ok((value & 0xff) as u8)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        for (offset, word) in data.iter_mut().enumerate() {
            *word = self.read32(address + (offset as u32))?;
        }

        Ok(())
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        for (offset, byte) in data.iter_mut().enumerate() {
            *byte = self.read8(address + (offset as u32))?;
        }

        Ok(())
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), crate::Error> {
        unimplemented!()
    }
}

struct AbstractCommandMemoryInterface {
    interface: RiscvCommunicationInterface,
}

impl MemoryInterface for AbstractCommandMemoryInterface {
    fn read32(&mut self, address: u32) -> Result<u32, crate::Error> {
        unimplemented!()
    }
    fn read8(&mut self, address: u32) -> Result<u8, crate::Error> {
        unimplemented!()
    }
    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        let mut abstractauto = Abstractauto(0);
        abstractauto.set_autoexecdata(1);
        self.interface.write_dm_register(abstractauto)?;

        self.interface.write_dm_register(Data1(address))?;

        let mut access_memory_command = AccessMemoryCommand(0);
        access_memory_command.set_aamsize(0);
        access_memory_command.set_aampostincrement(true);

        self.interface.write_dm_register(access_memory_command)?;

        for byte in data {
            let val: Data0 = self.interface.read_dm_register()?;

            *byte = (u32::from(val) & 0xff) as u8
        }

        // verify all commands worked
        let command_status: Abstractcs = self.interface.read_dm_register()?;

        if command_status.cmderr() == 0 {
            Ok(())
        } else {
            todo!("Proper error for command err: {:?}", command_status)
        }
    }
    fn write32(&mut self, addr: u32, data: u32) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), crate::Error> {
        unimplemented!()
    }
}

struct SystemBusAccessor {
    interface: RiscvCommunicationInterface,
}

impl MemoryInterface for SystemBusAccessor {
    fn read32(&mut self, address: u32) -> Result<u32, crate::Error> {
        let status: Sbcs = self.interface.read_dm_register()?;

        if !status.sbaccess32() {
            todo!("Proper error, access by system bus not supported");
        }

        log::debug!("sbcs: {:?}", status);

        let mut access_setup = Sbcs(0);
        access_setup.set_sbaccess(2);
        access_setup.set_sbreadondata(true);

        self.interface.write_dm_register(access_setup)?;

        self.interface.write_dm_register(Sbaddress0(address))?;

        // wait until sbbusy is low
        let mut status_busy: Sbcs = self.interface.read_dm_register()?;

        let repeat_count = 10;

        for _ in 0..repeat_count {
            status_busy = self.interface.read_dm_register()?;

            if !status_busy.sbbusy() {
                break;
            }
        }

        if status_busy.sbbusy() {
            todo!("Timeout, sbbusy still set...");
        }

        if status.sberror() != 0 {
            todo!("Error for sberror...");
        }

        let data: Sbdata0 = self.interface.read_dm_register()?;

        Ok(data.into())
    }

    fn read8(&mut self, address: u32) -> Result<u8, crate::Error> {
        let status: Sbcs = self.interface.read_dm_register()?;

        if !status.sbaccess8() {
            todo!("Proper error, access by system bus not supported");
        }

        log::debug!("sbcs: {:?}", status);

        let mut access_setup = Sbcs(0);
        access_setup.set_sbaccess(0);
        access_setup.set_sbreadondata(true);

        self.interface.write_dm_register(access_setup)?;

        self.interface.write_dm_register(Sbaddress0(address))?;

        // wait until sbbusy is low
        let mut status_busy: Sbcs = self.interface.read_dm_register()?;

        let repeat_count = 10;

        for _ in 0..repeat_count {
            status_busy = self.interface.read_dm_register()?;

            if !status_busy.sbbusy() {
                break;
            }
        }

        if status_busy.sbbusy() {
            todo!("Timeout, sbbusy still set...");
        }

        if status.sberror() != 0 {
            todo!("Error for sberror...");
        }

        let data: Sbdata0 = self.interface.read_dm_register()?;

        let value: u32 = data.into();

        Ok((value & 0xff) as u8)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }

    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        let status: Sbcs = self.interface.read_dm_register()?;

        if !status.sbaccess8() {
            todo!("Proper error, access by system bus not supported");
        }

        log::debug!("sbcs: {:?}", status);

        let mut access_setup = Sbcs(0);
        access_setup.set_sbaccess(0);
        access_setup.set_sbreadondata(true);
        access_setup.set_sbautoincrement(true);

        self.interface.write_dm_register(access_setup)?;

        self.interface.write_dm_register(Sbaddress0(address))?;

        for byte in data {
            // wait until sbbusy is low
            let mut status_busy: Sbcs = self.interface.read_dm_register()?;

            let repeat_count = 10;

            for _ in 0..repeat_count {
                status_busy = self.interface.read_dm_register()?;

                if !status_busy.sbbusy() {
                    break;
                }
            }

            if status_busy.sbbusy() {
                todo!("Timeout, sbbusy still set...");
            }

            if status.sberror() != 0 {
                todo!("Error for sberror...");
            }

            let register: Sbdata0 = self.interface.read_dm_register()?;

            let value: u32 = register.into();

            *byte = (value & 0xff) as u8;
        }

        Ok(())
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), crate::Error> {
        unimplemented!()
    }
}

fn dm_write_reg(address: u8, data: u32) -> u64 {
    ((address as u64) << 34) | ((data as u64) << 2) | 2
}

fn dm_read_reg(address: u8) -> u64 {
    ((address as u64) << 34) | 1
}

pub trait JTAGAccess {
    fn read_register(&mut self, address: u32, len: u32) -> Result<Vec<u8>, DebugProbeError>;
    fn write_register(
        &mut self,
        address: u32,
        data: &[u8],
        len: u32,
    ) -> Result<Vec<u8>, DebugProbeError>;
}

bitfield! {
    struct Dtmcs(u32);
    impl Debug;

    _, set_dmihardreset: 17;
    _, set_dmireset: 16;
    idle, _: 14, 12;
    dmistat, _: 11,10;
    abits, _: 9,4;
    version, _: 3,0;
}

bitfield! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec.
    pub struct AccessRegisterCommand(u32);
    impl Debug;
    pub _, set_cmd_type: 31, 24;
    pub _, set_aarsize: 22, 20;
    pub _, set_aarpostincrement: 19;
    pub _, set_postexec: 18;
    pub _, set_transfer: 17;
    pub _, set_write: 16;
    pub _, set_regno: 15, 0;
}

impl DebugRegister for AccessRegisterCommand {
    const ADDRESS: u8 = 0x17;
    const NAME: &'static str = "command";
}

impl From<AccessRegisterCommand> for u32 {
    fn from(register: AccessRegisterCommand) -> Self {
        register.0
    }
}

impl From<u32> for AccessRegisterCommand {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

pub(super) trait DebugRegister: Into<u32> + From<u32> {
    const ADDRESS: u8;
    const NAME: &'static str;
}

bitfield! {
    pub struct Sbcs(u32);
    impl Debug;

    sbversion, _: 31, 29;
    sbbusyerror, set_sbbusyerror: 22;
    sbbusy, _: 21;
    sbreadonaddr, set_sbreadonaddr: 20;
    sbaccess, set_sbaccess: 19, 17;
    sbautoincrement, set_sbautoincrement: 16;
    sbreadondata, set_sbreadondata: 16;
    sberror, set_sberror: 14, 12;
    sbasize, _: 11, 5;
    sbaccess128, _: 4;
    sbaccess64, _: 3;
    sbaccess32, _: 2;
    sbaccess16, _: 1;
    sbaccess8, _: 0;
}

impl DebugRegister for Sbcs {
    const ADDRESS: u8 = 0x38;
    const NAME: &'static str = "sbcs";
}

impl From<Sbcs> for u32 {
    fn from(register: Sbcs) -> Self {
        register.0
    }
}

bitfield! {
    pub struct Abstractauto(u32);
    impl Debug;

    autoexecprogbuf, set_autoexecprogbuf: 31, 16;
    autoexecdata, set_autoexecdata: 11, 0;
}

impl DebugRegister for Abstractauto {
    const ADDRESS: u8 = 0x38;
    const NAME: &'static str = "abstractauto";
}

impl From<Abstractauto> for u32 {
    fn from(register: Abstractauto) -> Self {
        register.0
    }
}

impl From<u32> for Abstractauto {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

impl From<u32> for Sbcs {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

bitfield! {
    /// Abstract command register, located at address 0x17
    /// This is not for all commands, only for the ones
    /// from the debug spec.
    pub struct AccessMemoryCommand(u32);
    impl Debug;
    _, set_cmd_type: 31, 24;
    pub _, set_aamvirtual: 23;
    pub _, set_aamsize: 22,20;
    pub _, set_aampostincrement: 19;
    pub _, set_write: 16;
    pub _, set_target_specific: 15, 14;
}

impl DebugRegister for AccessMemoryCommand {
    const ADDRESS: u8 = 0x17;
    const NAME: &'static str = "command";
}

impl From<AccessMemoryCommand> for u32 {
    fn from(register: AccessMemoryCommand) -> Self {
        let mut reg = register;
        reg.set_cmd_type(2);
        reg.0
    }
}

impl From<u32> for AccessMemoryCommand {
    fn from(value: u32) -> Self {
        Self(value)
    }
}

data_register! { Sbaddress0, 0x39, "sbaddress0" }
data_register! { Sbaddress1, 0x3a, "sbaddress1" }
data_register! { Sbaddress2, 0x3b, "sbaddress2" }
data_register! { Sbaddress3, 0x37, "sbaddress3" }

data_register! { Sbdata0, 0x3c, "sbdata0" }
data_register! { Sbdata1, 0x3d, "sbdata1" }
data_register! { Sbdata2, 0x3e, "sbdata2" }
data_register! { Sbdata3, 0x3f, "sbdata3" }
