//! Debug Module Communication
//!
//! This module implements communication with a
//! Debug Module, as described in the RISCV debug
//! specification v0.13.2 .

use super::{Dmcontrol, Dmstatus};
use crate::DebugProbeError;
use crate::Probe;

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

    pub fn read_dm_register(&self, address: u32) -> Result<u32, DebugProbeError> {
        self.inner.borrow_mut().read_dm_register(address)
    }

    pub fn write_dm_register(&self, address: u8, data: u32) -> Result<(), DebugProbeError> {
        self.inner.borrow_mut().write_dm_register(address, data)
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
        let status = Dmstatus(interface.read_dm_register(0x11)?);

        assert!(
            status.version() == 2,
            "Only Debug Module version 0.13 is supported!"
        );

        log::debug!("dmstatus: {:?}", status);

        // enable the debug module
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);

        interface.write_dm_register(0x11, control.0)?;

        Ok(interface)
    }

    /// Read the IDCODE register
    fn idcode(&self) -> u32 {
        todo!();
    }

    /// Read the `dtmcs` register
    fn read_dtmcs(&self) -> u32 {
        todo!();
    }

    fn dmi_hard_reset(&self) -> () {}

    fn dmi_reset(&self) -> () {}

    fn version(&self) -> () {}

    fn idle_cycles(&self) -> () {}

    pub fn read_dm_register(&mut self, address: u32) -> Result<u32, DebugProbeError> {
        log::debug!("Reading DM register at {:#010x}", address);

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dm_reg = dm_read_reg(address as u8);
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
            "Read DM register at {:#010x} = {:#010x}",
            address,
            response_value
        );

        Ok(response_value)
    }

    pub fn write_dm_register(&mut self, address: u8, data: u32) -> Result<(), DebugProbeError> {
        // write write command to dmi register

        log::debug!("Write DM register at {:#010x} = {:#010x}", address, data);

        let jtag_interface = self
            .probe
            .get_interface_jtag_mut()
            .ok_or(DebugProbeError::InterfaceNotAvailable("JTAG"))?;

        let dm_reg = dm_write_reg(address, data);

        let bytes = dm_reg.to_le_bytes();

        jtag_interface.write_register(0x11, &bytes[..6], 41)?;

        Ok(())
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
