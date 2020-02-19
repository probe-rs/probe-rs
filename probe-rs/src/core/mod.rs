pub(crate) mod communication_interface;

pub use communication_interface::CommunicationInterface;

use crate::config::TargetSelector;
use crate::error;
use crate::{
    architecture::{
        arm::{memory::ADIMemoryInterface, ArmCommunicationInterface},
        riscv::{communication_interface::RiscvCommunicationInterface, Riscv32},
    },
    DebugProbeError, Error, Memory, MemoryInterface, Probe,
};
use std::cell::RefCell;
use std::rc::Rc;

pub trait CoreRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

#[derive(Debug, Copy, Clone)]
pub struct CoreRegisterAddress(pub u16);

impl From<CoreRegisterAddress> for u32 {
    fn from(value: CoreRegisterAddress) -> Self {
        u32::from(value.0)
    }
}

impl From<u16> for CoreRegisterAddress {
    fn from(value: u16) -> Self {
        CoreRegisterAddress(value)
    }
}

#[allow(non_snake_case)]
#[derive(Copy, Clone)]
pub struct BasicRegisterAddresses {
    pub R0: CoreRegisterAddress,
    pub R1: CoreRegisterAddress,
    pub R2: CoreRegisterAddress,
    pub R3: CoreRegisterAddress,
    pub R4: CoreRegisterAddress,
    pub R5: CoreRegisterAddress,
    pub R6: CoreRegisterAddress,
    pub R7: CoreRegisterAddress,
    pub R8: CoreRegisterAddress,
    pub R9: CoreRegisterAddress,
    pub PC: CoreRegisterAddress,
    pub LR: CoreRegisterAddress,
    pub SP: CoreRegisterAddress,
    pub XPSR: CoreRegisterAddress,
}

#[derive(Debug, Clone)]
pub struct CoreInformation {
    pub pc: u32,
}

pub trait CoreInterface {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`] error will be returned.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    fn wait_for_core_halted(&self) -> Result<(), error::Error>;

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`CoreError::Timeout`] error will be returned.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    fn core_halted(&self) -> Result<bool, error::Error>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`CoreError::Timeout`] otherwise.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    fn halt(&self) -> Result<CoreInformation, error::Error>;

    fn run(&self) -> Result<(), error::Error>;

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: trait.Core.html#tymethod.reset_and_halt
    fn reset(&self) -> Result<(), error::Error>;

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: trait.Core.html#tymethod.reset
    fn reset_and_halt(&self) -> Result<CoreInformation, error::Error>;

    /// Steps one instruction and then enters halted state again.
    fn step(&self) -> Result<CoreInformation, error::Error>;

    fn read_core_reg(&self, address: CoreRegisterAddress) -> Result<u32, error::Error>;

    fn write_core_reg(&self, address: CoreRegisterAddress, value: u32) -> Result<(), error::Error>;

    fn get_available_breakpoint_units(&self) -> Result<u32, error::Error>;

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error>;

    fn set_breakpoint(&self, bp_unit_index: usize, addr: u32) -> Result<(), error::Error>;

    fn clear_breakpoint(&self, unit_index: usize) -> Result<(), error::Error>;

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses;

    fn memory(&self) -> Memory;
    fn hw_breakpoints_enabled(&self) -> bool;

    fn architecture(&self) -> Architecture;
}

impl MemoryInterface for Core {
    fn read32(&mut self, address: u32) -> Result<u32, Error> {
        self.memory().read32(address)
    }

    fn read8(&mut self, address: u32) -> Result<u8, Error> {
        self.memory().read8(address)
    }

    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        self.memory().read_block32(address, data)
    }
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.memory().read_block8(address, data)
    }

    fn write32(&mut self, addr: u32, data: u32) -> Result<(), Error> {
        self.memory().write32(addr, data)
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), Error> {
        self.memory().write8(addr, data)
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), Error> {
        self.memory().write_block32(addr, data)
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), Error> {
        self.memory().write_block8(addr, data)
    }
}

// dyn_clone::clone_trait_object!(CoreInterface);

// struct CoreVisitor;

// impl<'de> serde::de::Visitor<'de> for CoreVisitor {
//     type Value = Core;

//     fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
//         write!(formatter, "an existing core name")
//     }

//     fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
//     where
//         E: serde::de::Error,
//     {
//         if let Some(core) = get_core(s) {
//             Ok(core)
//         } else {
//             Err(Error::invalid_value(
//                 Unexpected::Other(&format!("Core {} does not exist.", s)),
//                 &self,
//             ))
//         }
//     }
// }

// impl<'de> serde::Deserialize<'de> for Box<dyn CoreInterface> {
//     fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
//         deserializer.deserialize_identifier(CoreVisitor)
//     }
// }

#[derive(Copy, Clone)]
pub enum CoreType {
    M4,
    M33,
    M0,
    Riscv,
}

impl CoreType {
    pub fn attach_arm(&self, interface: ArmCommunicationInterface) -> Result<Core, Error> {
        let memory = if let Some(memory) = interface.dedicated_memory_interface() {
            memory
        } else {
            // TODO: Change this to actually grab the proper memory IF.
            // For now always use the ARM IF.
            Memory::new(ADIMemoryInterface::<ArmCommunicationInterface>::new(
                interface, 0,
            ))
        };

        Ok(match self {
            CoreType::M4 => Core::new(crate::architecture::arm::m4::M4::new(memory)),
            CoreType::M33 => Core::new(crate::architecture::arm::m33::M33::new(memory)),
            CoreType::M0 => Core::new(crate::architecture::arm::m0::M0::new(memory)),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub fn attach_riscv(&self, interface: RiscvCommunicationInterface) -> Result<Core, Error> {
        Ok(match self {
            CoreType::Riscv => Core::new(Riscv32::new(interface)),
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub(crate) fn from_string(name: impl AsRef<str>) -> Option<Self> {
        match &name.as_ref().to_ascii_lowercase()[..] {
            "m0" => Some(CoreType::M0),
            "m4" => Some(CoreType::M4),
            "m33" => Some(CoreType::M33),
            "riscv" => Some(CoreType::Riscv),
            _ => None,
        }
    }
}

pub struct Core {
    inner: Rc<RefCell<dyn CoreInterface>>,
    breakpoints: Vec<Breakpoint>,
}

impl Core {
    pub fn new(core: impl CoreInterface + 'static) -> Self {
        Self {
            inner: Rc::new(RefCell::new(core)),
            breakpoints: Vec::new(),
        }
    }

    pub fn auto_attach(target: impl Into<TargetSelector>) -> Result<Core, error::Error> {
        // Get a list of all available debug probes.
        let probes = Probe::list_all();

        // Use the first probe found.
        let probe = probes[0].open()?;

        // Attach to a chip.
        let session = probe.attach(target)?;

        // Select a core.
        session.attach_to_core(0)
    }

    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`] error will be returned.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    pub fn wait_for_core_halted(&self) -> Result<(), error::Error> {
        self.inner.borrow().wait_for_core_halted()
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`CoreError::Timeout`] error will be returned.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    pub fn core_halted(&self) -> Result<bool, error::Error> {
        self.inner.borrow().core_halted()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`CoreError::Timeout`] otherwise.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    pub fn halt(&self) -> Result<CoreInformation, error::Error> {
        self.inner.borrow().halt()
    }

    pub fn run(&self) -> Result<(), error::Error> {
        self.inner.borrow().run()
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: trait.Core.html#tymethod.reset_and_halt
    pub fn reset(&self) -> Result<(), error::Error> {
        self.inner.borrow().reset()
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: trait.Core.html#tymethod.reset
    pub fn reset_and_halt(&self) -> Result<CoreInformation, error::Error> {
        self.inner.borrow().reset_and_halt()
    }

    /// Steps one instruction and then enters halted state again.
    pub fn step(&self) -> Result<CoreInformation, error::Error> {
        self.inner.borrow().step()
    }

    pub fn read_core_reg(
        &self,
        address: impl Into<CoreRegisterAddress>,
    ) -> Result<u32, error::Error> {
        self.inner.borrow().read_core_reg(address.into())
    }

    pub fn write_core_reg(
        &self,
        address: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), error::Error> {
        self.inner.borrow().write_core_reg(address, value)
    }

    pub fn get_available_breakpoint_units(&self) -> Result<u32, error::Error> {
        self.inner.borrow().get_available_breakpoint_units()
    }

    fn enable_breakpoints(&self, state: bool) -> Result<(), error::Error> {
        self.inner.borrow_mut().enable_breakpoints(state)
    }

    pub fn registers<'a>(&self) -> &'a BasicRegisterAddresses {
        self.inner.borrow().registers()
    }

    pub fn memory(&self) -> Memory {
        self.inner.borrow().memory()
    }

    pub fn read_word_32(&self, address: u32) -> Result<u32, error::Error> {
        self.inner.borrow_mut().memory().read32(address)
    }

    pub fn read_word_8(&self, address: u32) -> Result<u8, error::Error> {
        self.inner.borrow_mut().memory().read8(address)
    }

    pub fn read_32(&self, address: u32, data: &mut [u32]) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().read_block32(address, data)
    }

    pub fn read_8(&self, address: u32, data: &mut [u8]) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().read_block8(address, data)
    }

    pub fn write_word_32(&self, addr: u32, data: u32) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().write32(addr, data)
    }

    pub fn write_word_8(&self, addr: u32, data: u8) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().write8(addr, data)
    }

    pub fn write_32(&self, addr: u32, data: &[u32]) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().write_block32(addr, data)
    }

    pub fn write_8(&self, addr: u32, data: &[u8]) -> Result<(), error::Error> {
        self.inner.borrow_mut().memory().write_block8(addr, data)
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to set a hardware breakpoint. The amount
    /// of hardware breakpoints which are supported is chip specific,
    /// and can be queried using the `get_available_breakpoint_units` function.
    pub fn set_hw_breakpoint(&mut self, address: u32) -> Result<(), error::Error> {
        log::debug!("Trying to set HW breakpoint at address {:#08x}", address);

        // Get the number of HW breakpoints available
        let num_hw_breakpoints = self.get_available_breakpoint_units()? as usize;

        log::debug!("{} HW breakpoints are supported.", num_hw_breakpoints);

        if num_hw_breakpoints <= self.breakpoints.len() {
            // We cannot set additional breakpoints
            log::warn!("Maximum number of breakpoints ({}) reached, unable to set additional HW breakpoint.", num_hw_breakpoints);

            // TODO: Better error here
            return Err(error::Error::Probe(DebugProbeError::Unknown));
        }

        if !self.inner.borrow().hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        let bp_unit = self.find_free_breakpoint_unit();

        log::debug!("Using comparator {} of breakpoint unit", bp_unit);
        // actually set the breakpoint
        self.inner.borrow_mut().set_breakpoint(bp_unit, address)?;

        self.breakpoints.push(Breakpoint {
            address,
            register_hw: bp_unit,
        });

        Ok(())
    }

    pub fn clear_hw_breakpoint(&mut self, address: u32) -> Result<(), error::Error> {
        let bp_position = self.breakpoints.iter().position(|bp| bp.address == address);

        match bp_position {
            Some(bp_position) => {
                let bp = &self.breakpoints[bp_position];
                self.inner.borrow_mut().clear_breakpoint(bp.register_hw)?;

                // We only remove the breakpoint if we have actually managed to clear it.
                self.breakpoints.swap_remove(bp_position);
                Ok(())
            }
            None => Err(error::Error::Probe(DebugProbeError::Unknown)),
        }
    }

    fn find_free_breakpoint_unit(&self) -> usize {
        let mut used_bp: Vec<_> = self.breakpoints.iter().map(|bp| bp.register_hw).collect();
        used_bp.sort();

        let mut free_bp = 0;

        for bp in used_bp {
            if bp == free_bp {
                free_bp += 1;
            } else {
                return free_bp;
            }
        }

        free_bp
    }
}

pub struct CoreList(Vec<CoreType>);

impl CoreList {
    pub fn new(cores: Vec<CoreType>) -> Self {
        Self(cores)
    }
}

impl std::ops::Deref for CoreList {
    type Target = Vec<CoreType>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub struct BreakpointId(usize);

impl BreakpointId {
    pub fn new(id: usize) -> Self {
        BreakpointId(id)
    }
}

#[derive(Clone)]
pub struct Breakpoint {
    address: u32,
    register_hw: usize,
}

pub enum Architecture {
    ARM,
    RISCV,
}
