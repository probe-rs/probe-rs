//! AVR support
pub mod communication_interface;
use crate::architecture::avr::communication_interface::AvrCommunicationInterface;
use crate::{CoreInterface, MemoryInterface, CoreStatus, CoreInformation, CoreRegisterAddress, Architecture};
use crate::core::RegisterFile;
use crate::error::Error;
use crate::error;

use anyhow::{anyhow, Result};

use std::time::Duration;

pub struct Avr<'probe> {
    interface: &'probe mut AvrCommunicationInterface,
}
impl<'probe> Avr<'probe> {
    pub fn new(interface: &'probe mut AvrCommunicationInterface) -> Self {
        Self { interface }
    }
}

impl <'probe> CoreInterface for Avr<'probe> {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        unimplemented!();
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn core_halted(&mut self) -> Result<bool, error::Error> {
        unimplemented!();
    }

    fn status(&mut self) -> Result<CoreStatus, error::Error> {
        unimplemented!();
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [DebugProbeError::Timeout] otherwise.
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        unimplemented!();
    }

    fn run(&mut self) -> Result<(), error::Error> {
        unimplemented!();
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    fn reset(&mut self) -> Result<(), error::Error> {
        unimplemented!();
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        unimplemented!();
    }

    /// Steps one instruction and then enters halted state again.
    fn step(&mut self) -> Result<CoreInformation, error::Error> {
        unimplemented!();
    }

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, error::Error> {
        unimplemented!();
    }

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()> {
        unimplemented!()
    }

    fn get_available_breakpoint_units(&mut self) -> Result<u32, error::Error> {
        unimplemented!();
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error> {
        unimplemented!();
    }

    fn set_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), error::Error> {
        unimplemented!();
    }

    fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error> {
        unimplemented!();
    }

    fn registers(&self) -> &'static RegisterFile {
        unimplemented!();
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        unimplemented!();
    }

    /// Get the `Architecture` of the Core.
    fn architecture(&self) -> Architecture {
        unimplemented!();
    }
}
impl<'probe> MemoryInterface for Avr<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        //self.interface.read_word_32(address)
        unimplemented!()
    }
    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        //self.interface.read_word_8(address)
        unimplemented!()
    }
    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        //self.interface.read_32(address, data)
        unimplemented!()
    }
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        //self.interface.read_8(address, data)
        unimplemented!()
    }
    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        //self.interface.write_word_32(address, data)
        unimplemented!()
    }
    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        //self.interface.write_word_8(address, data)
        unimplemented!()
    }
    fn write_32(&mut self, address: u32, data: &[u32]) -> Result<(), Error> {
        //self.interface.write_32(address, data)
        unimplemented!()
    }
    fn write_8(&mut self, address: u32, data: &[u8]) -> Result<(), Error> {
        //self.interface.write_8(address, data)
        unimplemented!()
    }
    fn flush(&mut self) -> Result<(), Error> {
        //self.interface.flush()
        unimplemented!()
    }
}
