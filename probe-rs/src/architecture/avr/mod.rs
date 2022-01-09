/// AVR support
pub mod communication_interface;
use crate::architecture::avr::communication_interface::AvrCommunicationInterface;
use crate::core::{RegisterDescription, RegisterFile, RegisterKind};
use crate::error;
use crate::error::Error;
use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegisterAddress, CoreStatus, MemoryInterface,
};

use anyhow::Result;

use std::time::Duration;

static AVR_REGISTER_FILE: RegisterFile = RegisterFile {
    platform_registers: &[
        RegisterDescription {
            name: "R0",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(0),
        },
        RegisterDescription {
            name: "R1",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(1),
        },
        RegisterDescription {
            name: "R2",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(2),
        },
        RegisterDescription {
            name: "R3",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(3),
        },
        RegisterDescription {
            name: "R4",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(4),
        },
        RegisterDescription {
            name: "R5",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(5),
        },
        RegisterDescription {
            name: "R6",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(6),
        },
        RegisterDescription {
            name: "R7",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(7),
        },
        RegisterDescription {
            name: "R8",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(8),
        },
        RegisterDescription {
            name: "R9",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(9),
        },
        RegisterDescription {
            name: "R10",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(10),
        },
        RegisterDescription {
            name: "R11",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(11),
        },
        RegisterDescription {
            name: "R12",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(12),
        },
        RegisterDescription {
            name: "R13",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(13),
        },
        RegisterDescription {
            name: "R14",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(14),
        },
        RegisterDescription {
            name: "R15",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(15),
        },
        RegisterDescription {
            name: "R16",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(16),
        },
        RegisterDescription {
            name: "R17",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(17),
        },
        RegisterDescription {
            name: "R18",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(18),
        },
        RegisterDescription {
            name: "R19",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(19),
        },
        RegisterDescription {
            name: "R20",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(20),
        },
        RegisterDescription {
            name: "R21",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(21),
        },
        RegisterDescription {
            name: "R22",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(22),
        },
        RegisterDescription {
            name: "R23",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(23),
        },
        RegisterDescription {
            name: "R24",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(24),
        },
        RegisterDescription {
            name: "R25",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(25),
        },
        RegisterDescription {
            name: "R26",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(26),
        },
        RegisterDescription {
            name: "R27",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(27),
        },
        RegisterDescription {
            name: "R28",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(28),
        },
        RegisterDescription {
            name: "R29",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(29),
        },
        RegisterDescription {
            name: "R30",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(30),
        },
        RegisterDescription {
            name: "R31",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(31),
        },
        RegisterDescription {
            name: "SREG",
            _kind: RegisterKind::General,
            address: CoreRegisterAddress(32),
        },
    ],

    program_counter: &RegisterDescription {
        name: "PC",
        _kind: RegisterKind::PC,
        address: CoreRegisterAddress(34),
    },
    return_address: &RegisterDescription {
        name: "RA",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(0),
    },
    stack_pointer: &RegisterDescription {
        name: "SP",
        _kind: RegisterKind::General,
        address: CoreRegisterAddress(33),
    },

    argument_registers: &[],
    result_registers: &[],
    extra: None,
    msp: None,
    psp: None,
};

pub struct Avr<'probe> {
    interface: &'probe mut AvrCommunicationInterface,
}
impl<'probe> Avr<'probe> {
    pub fn new(interface: &'probe mut AvrCommunicationInterface) -> Self {
        Self { interface }
    }
}

impl<'probe> CoreInterface for Avr<'probe> {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        unimplemented!();
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn core_halted(&mut self) -> Result<bool, error::Error> {
        Ok(match self.interface.status()? {
            CoreStatus::Halted(_) => true,
            _ => false,
        })
    }

    fn status(&mut self) -> Result<CoreStatus, error::Error> {
        self.interface.status()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [DebugProbeError::Timeout] otherwise.
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.interface.halt(timeout)
    }

    fn run(&mut self) -> Result<(), error::Error> {
        self.interface.run()
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
        self.interface.reset_and_halt(timeout)
    }

    /// Steps one instruction and then enters halted state again.
    fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.interface.step()
    }

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, error::Error> {
        match address.0 {
            32 => Ok(self.interface.read_sreg()?),
            33 => Ok(self.interface.read_stack_pointer()?),
            34 => Ok(self.interface.read_program_counter()?),
            _ => Ok(self.interface.read_register(address.into())? as u32),
        }
    }

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()> {
        unimplemented!()
    }

    fn get_available_breakpoint_units(&mut self) -> Result<u32, error::Error> {
        //FIXME: Add support for SW breakpoints and devices with more than one hw breakpoint
        Ok(1)
    }

    fn get_hw_breakpoints(&mut self) -> Result<Vec<Option<u32>>, error::Error>{
        todo!();
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error> {
        unimplemented!();
    }

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), error::Error>{
        todo!();
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error>{
        todo!();
    }

    fn registers(&self) -> &'static RegisterFile {
        &AVR_REGISTER_FILE
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        unimplemented!();
    }

    /// Get the `Architecture` of the Core.
    fn architecture(&self) -> Architecture {
        Architecture::Avr
    }
}
impl<'probe> MemoryInterface for Avr<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        //self.interface.read_word_32(address)
        unimplemented!()
    }
    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        self.interface.read_word_8(address)
    }
    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        //self.interface.read_32(address, data)
        unimplemented!()
    }
    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.interface.read_8(address, data)
    }
    fn write_word_32(&mut self, address: u32, data: u32) -> Result<(), Error> {
        //self.interface.write_word_32(address, data)
        unimplemented!()
    }
    fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), Error> {
        self.interface.write_word_8(address, data)
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
