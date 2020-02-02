//! RISCV Support

use crate::core::Architecture;
use crate::{CoreInterface, Probe};
use communication_interface::RiscvCommunicationInterface;

use crate::core::{CoreInformation, RegisterFile};
use crate::CoreRegisterAddress;
use bitfield::bitfield;

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
            let dmstatus = Dmstatus(self.interface.read_dm_register(0x11)?);

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
        let current_dmcontrol = Dmcontrol(self.interface.read_dm_register(0x10)?);
        log::debug!("{:?}", current_dmcontrol);

        let mut dmcontrol = Dmcontrol(0);

        dmcontrol.set_haltreq(true);
        dmcontrol.set_dmactive(true);

        self.interface.write_dm_register(0x10, dmcontrol.0)?;

        self.wait_for_core_halted()?;

        let pc = self.read_core_reg(CoreRegisterAddress(0x7b1))?;

        Ok(CoreInformation { pc })
    }

    fn run(&self) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn reset(&self) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn reset_and_halt(&self) -> Result<crate::core::CoreInformation, crate::Error> {
        unimplemented!()
    }
    fn step(&self) -> Result<crate::core::CoreInformation, crate::Error> {
        unimplemented!()
    }
    fn read_core_reg(&self, address: crate::CoreRegisterAddress) -> Result<u32, crate::Error> {
        unimplemented!()
    }
    fn write_core_reg(
        &self,
        address: crate::CoreRegisterAddress,
        value: u32,
    ) -> Result<(), crate::Error> {
        unimplemented!()
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
        unimplemented!()
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
