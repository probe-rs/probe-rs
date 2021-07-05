use crate::architecture::{
    arm::core::CortexState, riscv::communication_interface::RiscvCommunicationInterface,
};
use crate::config::CoreType;
use crate::{error, Error, Memory, MemoryInterface};
use anyhow::{anyhow, Result};
use std::time::Duration;

pub trait CoreRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
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
#[derive(Debug, Clone)]
pub struct CoreInformation {
    pub pc: u32,
}

#[derive(Debug, Clone)]
pub struct RegisterDescription {
    pub(crate) name: &'static str,
    pub(crate) kind: RegisterKind,
    pub(crate) address: CoreRegisterAddress,
}

impl RegisterDescription {
    pub fn name(&self) -> &'static str {
        self.name
    }
}

impl From<RegisterDescription> for CoreRegisterAddress {
    fn from(description: RegisterDescription) -> CoreRegisterAddress {
        description.address
    }
}

impl From<&RegisterDescription> for CoreRegisterAddress {
    fn from(description: &RegisterDescription) -> CoreRegisterAddress {
        description.address
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RegisterKind {
    General,
    PC,
}

/// Register description for a core.

#[derive(Debug)]
pub struct RegisterFile {
    pub(crate) platform_registers: &'static [RegisterDescription],

    pub(crate) program_counter: &'static RegisterDescription,

    pub(crate) stack_pointer: &'static RegisterDescription,

    pub(crate) return_address: &'static RegisterDescription,

    pub(crate) argument_registers: &'static [RegisterDescription],
    pub(crate) result_registers: &'static [RegisterDescription],
}

impl RegisterFile {
    pub fn registers(&self) -> impl Iterator<Item = &RegisterDescription> {
        self.platform_registers.iter()
    }

    pub fn program_counter(&self) -> &RegisterDescription {
        &self.program_counter
    }

    pub fn stack_pointer(&self) -> &RegisterDescription {
        &self.stack_pointer
    }

    pub fn return_address(&self) -> &RegisterDescription {
        &self.return_address
    }

    pub fn argument_register(&self, index: usize) -> &RegisterDescription {
        &self.argument_registers[index]
    }

    pub fn get_argument_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.argument_registers.get(index)
    }

    pub fn result_register(&self, index: usize) -> &RegisterDescription {
        &self.result_registers[index]
    }

    pub fn get_result_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.result_registers.get(index)
    }

    pub fn platform_register(&self, index: usize) -> &RegisterDescription {
        &self.platform_registers[index]
    }

    pub fn get_platform_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.platform_registers.get(index)
    }
}

pub trait CoreInterface: MemoryInterface {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error>;

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    fn core_halted(&mut self) -> Result<bool, error::Error>;

    fn status(&mut self) -> Result<CoreStatus, error::Error>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [DebugProbeError::Timeout] otherwise.
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error>;

    fn run(&mut self) -> Result<(), error::Error>;

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    fn reset(&mut self) -> Result<(), error::Error>;

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error>;

    /// Steps one instruction and then enters halted state again.
    fn step(&mut self) -> Result<CoreInformation, error::Error>;

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, error::Error>;

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()>;

    fn get_available_breakpoint_units(&mut self) -> Result<u32, error::Error>;

    /// Read the hardware breakpoints from FpComp registers, and adds them to the Result Vector.
    /// A value of None in any position of the Vector indicates that the position is unset/available.
    /// We intentionally return all breakpoints, irrespective of whether they are enabled or not.
    fn get_hw_breakpoints(&mut self) -> Result<Vec<Option<u32>>, error::Error>;

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error>;

    fn set_hw_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), error::Error>;

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error>;

    fn registers(&self) -> &'static RegisterFile;

    fn hw_breakpoints_enabled(&self) -> bool;

    /// Get the `Architecture` of the Core.
    fn architecture(&self) -> Architecture;
}

impl<'probe> MemoryInterface for Core<'probe> {
    fn read_word_32(&mut self, address: u32) -> Result<u32, Error> {
        self.inner.read_word_32(address)
    }

    fn read_word_8(&mut self, address: u32) -> Result<u8, Error> {
        self.inner.read_word_8(address)
    }

    fn read_32(&mut self, address: u32, data: &mut [u32]) -> Result<(), Error> {
        self.inner.read_32(address, data)
    }

    fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), Error> {
        self.inner.read_8(address, data)
    }

    fn write_word_32(&mut self, addr: u32, data: u32) -> Result<(), Error> {
        self.inner.write_word_32(addr, data)
    }

    fn write_word_8(&mut self, addr: u32, data: u8) -> Result<(), Error> {
        self.inner.write_word_8(addr, data)
    }

    fn write_32(&mut self, addr: u32, data: &[u32]) -> Result<(), Error> {
        self.inner.write_32(addr, data)
    }

    fn write_8(&mut self, addr: u32, data: &[u8]) -> Result<(), Error> {
        self.inner.write_8(addr, data)
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.inner.flush()
    }
}

#[derive(Debug)]
pub struct CoreState {
    id: usize,
}

impl CoreState {
    pub fn new(id: usize) -> Self {
        Self { id }
    }
}

#[derive(Debug)]
pub enum SpecificCoreState {
    M3(CortexState),
    M4(CortexState),
    M33(CortexState),
    M0(CortexState),
    M7(CortexState),
    Riscv,
}

impl SpecificCoreState {
    pub(crate) fn from_core_type(typ: CoreType) -> Self {
        match typ {
            CoreType::M0 => SpecificCoreState::M0(CortexState::new()),
            CoreType::M3 => SpecificCoreState::M3(CortexState::new()),
            CoreType::M33 => SpecificCoreState::M33(CortexState::new()),
            CoreType::M4 => SpecificCoreState::M4(CortexState::new()),
            CoreType::M7 => SpecificCoreState::M7(CortexState::new()),
            CoreType::Riscv => SpecificCoreState::Riscv,
        }
    }

    pub(crate) fn core_type(&self) -> CoreType {
        match self {
            SpecificCoreState::M0(_) => CoreType::M0,
            SpecificCoreState::M3(_) => CoreType::M3,
            SpecificCoreState::M33(_) => CoreType::M33,
            SpecificCoreState::M4(_) => CoreType::M4,
            SpecificCoreState::M7(_) => CoreType::M7,
            SpecificCoreState::Riscv => CoreType::Riscv,
        }
    }

    pub(crate) fn attach_arm<'probe>(
        &'probe mut self,
        state: &'probe mut CoreState,
        memory: Memory<'probe>,
    ) -> Result<Core<'probe>, Error> {
        Ok(match self {
            // TODO: Change this once the new archtecture structure for ARM hits.
            // Cortex-M3, M4 and M7 use the Armv7[E]-M architecture and are
            // identical for our purposes.
            SpecificCoreState::M3(s) | SpecificCoreState::M4(s) | SpecificCoreState::M7(s) => {
                Core::new(crate::architecture::arm::m4::M4::new(memory, s)?, state)
            }
            SpecificCoreState::M33(s) => {
                Core::new(crate::architecture::arm::m33::M33::new(memory, s)?, state)
            }
            SpecificCoreState::M0(s) => {
                Core::new(crate::architecture::arm::m0::M0::new(memory, s)?, state)
            }
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }

    pub fn attach_riscv<'probe>(
        &self,
        state: &'probe mut CoreState,
        interface: &'probe mut RiscvCommunicationInterface,
    ) -> Result<Core<'probe>, Error> {
        Ok(match self {
            SpecificCoreState::Riscv => {
                Core::new(crate::architecture::riscv::Riscv32::new(interface), state)
            }
            _ => {
                return Err(Error::UnableToOpenProbe(
                    "Core architecture and Probe mismatch.",
                ))
            }
        })
    }
}

pub struct Core<'probe> {
    inner: Box<dyn CoreInterface + 'probe>,
    state: &'probe mut CoreState,
}

impl<'probe> Core<'probe> {
    pub fn new(core: impl CoreInterface + 'probe, state: &'probe mut CoreState) -> Core<'probe> {
        Self {
            inner: Box::new(core),
            state,
        }
    }

    pub fn create_state(id: usize) -> CoreState {
        CoreState::new(id)
    }

    pub fn id(&self) -> usize {
        self.state.id
    }

    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        self.inner.wait_for_core_halted(timeout)
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [DebugProbeError::Timeout] error will be returned.
    pub fn core_halted(&mut self) -> Result<bool, error::Error> {
        self.inner.core_halted()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [DebugProbeError::Timeout] otherwise.
    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.inner.halt(timeout)
    }

    pub fn run(&mut self) -> Result<(), error::Error> {
        self.inner.run()
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    pub fn reset(&mut self) -> Result<(), error::Error> {
        self.inner.reset()
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.inner.reset_and_halt(timeout)
    }

    /// Steps one instruction and then enters halted state again.
    pub fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.inner.step()
    }

    pub fn status(&mut self) -> Result<CoreStatus, error::Error> {
        self.inner.status()
    }

    pub fn read_core_reg(
        &mut self,
        address: impl Into<CoreRegisterAddress>,
    ) -> Result<u32, error::Error> {
        self.inner.read_core_reg(address.into())
    }

    pub fn write_core_reg(
        &mut self,
        address: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), error::Error> {
        Ok(self.inner.write_core_reg(address, value)?)
    }

    pub fn get_available_breakpoint_units(&mut self) -> Result<u32, error::Error> {
        self.inner.get_available_breakpoint_units()
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error> {
        self.inner.enable_breakpoints(state)
    }

    pub fn registers(&self) -> &'static RegisterFile {
        self.inner.registers()
    }

    /// Find the index of the next available HW breakpoint comparator.
    fn find_free_breakpoint_comparator_index(&mut self) -> Result<usize, error::Error> {
        let mut next_available_hw_breakpoint = 0;
        for breakpoint in self.inner.get_hw_breakpoints()? {
            if breakpoint.is_none() {
                return Ok(next_available_hw_breakpoint);
            } else {
                next_available_hw_breakpoint += 1;
            }
        }
        Err(error::Error::Other(anyhow!(
            "No available hardware breakpoints"
        )))
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to set a hardware breakpoint. The amount
    /// of hardware breakpoints which are supported is chip specific,
    /// and can be queried using the `get_available_breakpoint_units` function.
    pub fn set_hw_breakpoint(&mut self, address: u32) -> Result<(), error::Error> {
        if !self.inner.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        //If there is a breakpoint set already, return its bp_unit_index, else find the next free index
        let breakpoint_comparator_index = match self
            .inner
            .get_hw_breakpoints()?
            .iter()
            .position(|&bp| bp == Some(address))
        {
            Some(breakpoint_comparator_index) => breakpoint_comparator_index,
            None => self.find_free_breakpoint_comparator_index()?,
        };

        log::debug!(
            "Trying to set HW breakpoint #{} with comparator address  {:#08x}",
            breakpoint_comparator_index,
            address
        );

        // Actually set the breakpoint. Even if it has been set, set it again so it will be active.
        self.inner
            .set_hw_breakpoint(breakpoint_comparator_index, address)?;
        Ok(())
    }

    pub fn clear_hw_breakpoint(&mut self, address: u32) -> Result<(), error::Error> {
        let bp_position = self
            .inner
            .get_hw_breakpoints()?
            .iter()
            .position(|bp| bp.is_some() && bp.unwrap() == address);

        log::debug!(
            "Will clear HW breakpoint    #{} with comparator address    {:#08x}",
            bp_position.unwrap_or(usize::MAX),
            address
        );

        match bp_position {
            Some(bp_position) => {
                self.inner.clear_hw_breakpoint(bp_position)?;
                Ok(())
            }
            None => Err(error::Error::Other(anyhow!(
                "No breakpoint found at address {}",
                address
            ))),
        }
    }

    /// Clear all hardware breakpoints
    ///
    /// This function will clear all HW breakpoints which are configured on the target,
    /// regardless if they are set by probe-rs, AND regardless if they are enabled or not.
    /// Also used as a helper function in [`Session::drop`].
    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), error::Error> {
        for breakpoint in (self.inner.get_hw_breakpoints()?).into_iter().flatten() {
            self.clear_hw_breakpoint(breakpoint)?
        }
        Ok(())
    }

    pub fn architecture(&self) -> Architecture {
        self.inner.architecture()
    }
}

pub struct CoreList<'probe>(&'probe [CoreType]);

impl<'probe> CoreList<'probe> {
    pub fn new(cores: &'probe [CoreType]) -> Self {
        Self(cores)
    }
}

impl<'probe> std::ops::Deref for CoreList<'probe> {
    type Target = [CoreType];
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

#[derive(Clone, Debug)]
pub struct Breakpoint {
    address: u32,
    register_hw: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Architecture {
    Arm,
    Riscv,
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum CoreStatus {
    Running,
    Halted(HaltReason),
    /// This is a Cortex-M specific status, and will not be set or handled by RISCV code.
    LockedUp,
    Sleeping,
    Unknown,
}

impl CoreStatus {
    pub fn is_halted(&self) -> bool {
        matches!(self, CoreStatus::Halted(_))
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
pub enum HaltReason {
    /// Multiple reasons for a halt.
    ///
    /// This can happen for example when a single instruction
    /// step ends up on a breakpoint, after which both breakpoint and step / request
    /// are set.
    Multiple,
    /// Core halted due to a breakpoint, either
    /// a *soft* or a *hard* breakpoint.
    Breakpoint,
    /// Core halted due to an exception, e.g. an
    /// an interrupt.
    Exception,
    /// Core halted due to a data watchpoint
    Watchpoint,
    /// Core halted after single step
    Step,
    /// Core halted because of a debugger request
    Request,
    /// External halt request
    External,
    /// Unknown reason for halt.
    ///
    /// This can happen for example when the core is already halted when we connect.
    Unknown,
}
