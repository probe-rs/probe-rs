pub(crate) mod communication_interface;

pub use communication_interface::CommunicationInterface;

use crate::error;
use crate::DebugProbeError;
use crate::{
    architecture::{
        arm::core::CortexState, riscv::communication_interface::RiscvCommunicationInterface,
    },
    Error, Memory, MemoryInterface,
};
use anyhow::{anyhow, Result};
use std::time::Duration;

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
    /// a [`DebugProbeError::Timeout`] error will be returned.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error>;

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`CoreError::Timeout`] error will be returned.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    fn core_halted(&mut self) -> Result<bool, error::Error>;

    fn status(&mut self) -> Result<CoreStatus, error::Error>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`CoreError::Timeout`] otherwise.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error>;

    fn run(&mut self) -> Result<(), error::Error>;

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: trait.Core.html#tymethod.reset_and_halt
    fn reset(&mut self) -> Result<(), error::Error>;

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: trait.Core.html#tymethod.reset
    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error>;

    /// Steps one instruction and then enters halted state again.
    fn step(&mut self) -> Result<CoreInformation, error::Error>;

    fn read_core_reg(&mut self, address: CoreRegisterAddress) -> Result<u32, error::Error>;

    fn write_core_reg(&mut self, address: CoreRegisterAddress, value: u32) -> Result<()>;

    fn get_available_breakpoint_units(&mut self) -> Result<u32, error::Error>;

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error>;

    fn set_breakpoint(&mut self, bp_unit_index: usize, addr: u32) -> Result<(), error::Error>;

    fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error>;

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

#[derive(Copy, Clone)]
pub enum CoreType {
    M3,
    M4,
    M33,
    M0,
    M7,
    Riscv,
}

impl CoreType {
    pub(crate) fn from_string(name: impl AsRef<str>) -> Option<Self> {
        match &name.as_ref().to_ascii_lowercase()[..] {
            "m0" => Some(CoreType::M0),
            "m4" => Some(CoreType::M4),
            "m3" => Some(CoreType::M3),
            "m33" => Some(CoreType::M33),
            "riscv" => Some(CoreType::Riscv),
            "m7" => Some(CoreType::M7),
            _ => None,
        }
    }

    pub(crate) fn from(value: &SpecificCoreState) -> Self {
        match value {
            SpecificCoreState::M0(_) => CoreType::M0,
            SpecificCoreState::M3(_) => CoreType::M3,
            SpecificCoreState::M33(_) => CoreType::M33,
            SpecificCoreState::M4(_) => CoreType::M4,
            SpecificCoreState::M7(_) => CoreType::M7,
            SpecificCoreState::Riscv => CoreType::Riscv,
        }
    }
}

#[derive(Debug)]
pub struct CoreState {
    id: usize,
    breakpoints: Vec<Breakpoint>,
}

impl CoreState {
    fn new(id: usize) -> Self {
        Self {
            id,
            breakpoints: vec![],
        }
    }
}

#[derive(Debug)]
pub(crate) enum SpecificCoreState {
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

    pub(crate) fn attach_riscv<'probe>(
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
    /// a [`DebugProbeError::Timeout`] error will be returned.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        self.inner.wait_for_core_halted(timeout)
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`CoreError::Timeout`] error will be returned.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    pub fn core_halted(&mut self) -> Result<bool, error::Error> {
        self.inner.core_halted()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`CoreError::Timeout`] otherwise.
    ///
    /// [`CoreError::Timeout`]: ../probe/debug_probe/enum.CoreError.html#variant.Timeout
    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.inner.halt(timeout)
    }

    pub fn run(&mut self) -> Result<(), error::Error> {
        self.inner.run()
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: trait.Core.html#tymethod.reset_and_halt
    pub fn reset(&mut self) -> Result<(), error::Error> {
        self.inner.reset()
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: trait.Core.html#tymethod.reset
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

        if num_hw_breakpoints <= self.state.breakpoints.len() {
            // We cannot set additional breakpoints
            log::warn!("Maximum number of breakpoints ({}) reached, unable to set additional HW breakpoint.", num_hw_breakpoints);

            return Err(error::Error::Probe(
                DebugProbeError::BreakpointUnitsExceeded,
            ));
        }

        if !self.inner.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        let bp_unit = self.find_free_breakpoint_unit();

        log::debug!("Using comparator {} of breakpoint unit", bp_unit);
        // actually set the breakpoint
        self.inner.set_breakpoint(bp_unit, address)?;

        self.state.breakpoints.push(Breakpoint {
            address,
            register_hw: bp_unit,
        });

        Ok(())
    }

    pub fn clear_hw_breakpoint(&mut self, address: u32) -> Result<(), error::Error> {
        let bp_position = self
            .state
            .breakpoints
            .iter()
            .position(|bp| bp.address == address);

        match bp_position {
            Some(bp_position) => {
                let bp = &self.state.breakpoints[bp_position];
                self.inner.clear_breakpoint(bp.register_hw)?;

                // We only remove the breakpoint if we have actually managed to clear it.
                self.state.breakpoints.swap_remove(bp_position);
                Ok(())
            }
            None => Err(error::Error::Other(anyhow!(
                "No breakpoint found at address {}",
                address
            ))),
        }
    }

    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), error::Error> {
        let num_hw_breakpoints = self.get_available_breakpoint_units()? as usize;

        { 0..num_hw_breakpoints }
            .map(|unit_index| self.inner.clear_breakpoint(unit_index))
            .collect()
    }

    pub fn architecture(&self) -> Architecture {
        self.inner.architecture()
    }

    fn find_free_breakpoint_unit(&self) -> usize {
        let mut used_bp: Vec<_> = self
            .state
            .breakpoints
            .iter()
            .map(|bp| bp.register_hw)
            .collect();
        used_bp.sort_unstable();

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
    /// Unknown reason for halt. This can happen for
    /// example when the core is already halted when we connect.
    Unknown,
}
