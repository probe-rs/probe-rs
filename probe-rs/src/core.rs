use crate::{
    CoreType, Endian, InstructionSet, MemoryInterface, Target,
    architecture::{
        arm::sequences::ArmDebugSequence, riscv::sequences::RiscvDebugSequence,
        xtensa::sequences::XtensaDebugSequence,
    },
    config::DebugSequence,
    error::Error,
    memory::CoreMemoryInterface,
};
pub use probe_rs_target::{Architecture, CoreAccessOptions};
use probe_rs_target::{
    ArmCoreAccessOptions, MemoryRegion, RiscvCoreAccessOptions, XtensaCoreAccessOptions,
};
use std::{sync::Arc, time::Duration};

pub mod core_state;
pub mod core_status;
pub mod dump;
pub mod memory_mapped_registers;
pub mod registers;

pub use core_state::*;
pub use core_status::*;
pub use memory_mapped_registers::MemoryMappedRegister;
pub use registers::*;

/// An struct for storing the current state of a core.
#[derive(Debug, Clone)]
pub struct CoreInformation {
    /// The current Program Counter.
    pub pc: u64,
}

/// A generic interface to control a MCU core.
pub trait CoreInterface: MemoryInterface {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) error will be returned.
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error>;

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) error will be returned.
    fn core_halted(&mut self) -> Result<bool, Error>;

    /// Returns the current status of the core.
    fn status(&mut self) -> Result<CoreStatus, Error>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) otherwise.
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error>;

    /// Continue to execute instructions.
    fn run(&mut self) -> Result<(), Error>;

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    fn reset(&mut self) -> Result<(), Error>;

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error>;

    /// Steps one instruction and then enters halted state again.
    fn step(&mut self) -> Result<CoreInformation, Error>;

    /// Read the value of a core register.
    fn read_core_reg(
        &mut self,
        address: registers::RegisterId,
    ) -> Result<registers::RegisterValue, Error>;

    /// Write the value of a core register.
    fn write_core_reg(
        &mut self,
        address: registers::RegisterId,
        value: registers::RegisterValue,
    ) -> Result<(), Error>;

    /// Returns all the available breakpoint units of the core.
    fn available_breakpoint_units(&mut self) -> Result<u32, Error>;

    /// Read the hardware breakpoints from FpComp registers, and adds them to the Result Vector.
    /// A value of None in any position of the Vector indicates that the position is unset/available.
    /// We intentionally return all breakpoints, irrespective of whether they are enabled or not.
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error>;

    /// Enables breakpoints on this core. If a breakpoint is set, it will halt as soon as it is hit.
    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error>;

    /// Sets a breakpoint at `addr`. It does so by using unit `bp_unit_index`.
    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error>;

    /// Clears the breakpoint configured in unit `unit_index`.
    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error>;

    /// Returns a list of all the registers of this core.
    fn registers(&self) -> &'static registers::CoreRegisters;

    /// Returns the program counter register.
    fn program_counter(&self) -> &'static CoreRegister;

    /// Returns the stack pointer register.
    fn frame_pointer(&self) -> &'static CoreRegister;

    /// Returns the frame pointer register.
    fn stack_pointer(&self) -> &'static CoreRegister;

    /// Returns the return address register, a.k.a. link register.
    fn return_address(&self) -> &'static CoreRegister;

    /// Returns `true` if hardware breakpoints are enabled, `false` otherwise.
    fn hw_breakpoints_enabled(&self) -> bool;

    /// Configure the target to ensure software breakpoints will enter Debug Mode.
    fn debug_on_sw_breakpoint(&mut self, _enabled: bool) -> Result<(), Error> {
        // This default will have override methods for architectures that require special behavior, e.g. RISC-V.
        Ok(())
    }

    /// Get the `Architecture` of the Core.
    fn architecture(&self) -> Architecture;

    /// Get the `CoreType` of the Core
    fn core_type(&self) -> CoreType;

    /// Determine the instruction set the core is operating in
    /// This must be queried while halted as this is a runtime
    /// decision for some core types
    fn instruction_set(&mut self) -> Result<InstructionSet, Error>;

    /// Return which endianness the core is currently in. Some cores
    /// allow for changing the endianness.
    fn endianness(&mut self) -> Result<Endian, Error> {
        // Return little endian, since that is the most common mode by far.
        Ok(Endian::Little)
    }

    /// Determine if an FPU is present.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    fn fpu_support(&mut self) -> Result<bool, Error>;

    /// Determine the number of floating point registers.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    fn floating_point_register_count(&mut self) -> Result<usize, Error>;

    /// Set the reset catch setting.
    ///
    /// This configures the core to halt after a reset.
    ///
    /// use `reset_catch_clear` to clear the setting again.
    fn reset_catch_set(&mut self) -> Result<(), Error>;

    /// Clear the reset catch setting.
    ///
    /// This will reset the changes done by `reset_catch_set`.
    fn reset_catch_clear(&mut self) -> Result<(), Error>;

    /// Called when we stop debugging a core.
    fn debug_core_stop(&mut self) -> Result<(), Error>;

    /// Called during session stop to do any pending cleanup
    fn on_session_stop(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Enables vector catching for the given `condition`
    fn enable_vector_catch(&mut self, _condition: VectorCatchCondition) -> Result<(), Error> {
        Err(Error::NotImplemented("vector catch"))
    }

    /// Disables vector catching for the given `condition`
    fn disable_vector_catch(&mut self, _condition: VectorCatchCondition) -> Result<(), Error> {
        Err(Error::NotImplemented("vector catch"))
    }

    /// Check if the integer size is 64-bit
    fn is_64_bit(&self) -> bool {
        false
    }
}

/// Generic core handle representing a physical core on an MCU.
///
/// This should be considered as a temporary view of the core which locks the debug probe driver to as single consumer by borrowing it.
///
/// As soon as you did your atomic task (e.g. halt the core, read the core state and all other debug relevant info) you should drop this object,
/// to allow potential other shareholders of the session struct to grab a core handle too.
pub struct Core<'probe> {
    id: usize,
    name: &'probe str,
    target: &'probe Target,

    inner: Box<dyn CoreInterface + 'probe>,
}

impl CoreMemoryInterface for Core<'_> {
    type ErrorType = Error;

    fn memory(&self) -> &dyn MemoryInterface<Self::ErrorType> {
        self.inner.as_ref()
    }

    fn memory_mut(&mut self) -> &mut dyn MemoryInterface<Self::ErrorType> {
        self.inner.as_mut()
    }
}

impl<'probe> Core<'probe> {
    /// Borrow the boxed CoreInterface mutable.
    pub fn inner_mut(&mut self) -> &mut Box<dyn CoreInterface + 'probe> {
        &mut self.inner
    }

    /// Create a new [`Core`].
    pub(crate) fn new(
        id: usize,
        name: &'probe str,
        target: &'probe Target,
        core: impl CoreInterface + 'probe,
    ) -> Core<'probe> {
        Self {
            id,
            name,
            target,
            inner: Box::new(core),
        }
    }

    /// Returns the memory regions associated with this core.
    pub fn memory_regions(&self) -> impl Iterator<Item = &MemoryRegion> {
        self.target
            .memory_map
            .iter()
            .filter(|r| r.cores().iter().any(|m| m == self.name))
    }

    /// Returns the target descriptor of the current `Session`.
    pub fn target(&self) -> &Target {
        self.target
    }

    /// Creates a new [`CoreState`]
    pub(crate) fn create_state(
        id: usize,
        options: CoreAccessOptions,
        target: &Target,
        core_type: CoreType,
    ) -> CombinedCoreState {
        CombinedCoreState {
            id,
            core_state: CoreState::new(ResolvedCoreOptions::new(target, options)),
            specific_state: SpecificCoreState::from_core_type(core_type),
        }
    }

    /// Returns the ID of this core.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) error will be returned.
    #[tracing::instrument(skip(self))]
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.inner.wait_for_core_halted(timeout)
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) error will be returned.
    pub fn core_halted(&mut self) -> Result<bool, Error> {
        self.inner.core_halted()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`DebugProbeError::Timeout`](crate::probe::DebugProbeError::Timeout) otherwise.
    #[tracing::instrument(skip(self))]
    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.inner.halt(timeout)
    }

    /// Continue to execute instructions.
    #[tracing::instrument(skip(self))]
    pub fn run(&mut self) -> Result<(), Error> {
        self.inner.run()
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    #[tracing::instrument(skip(self))]
    pub fn reset(&mut self) -> Result<(), Error> {
        self.inner.reset()
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    #[tracing::instrument(skip(self))]
    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.inner.reset_and_halt(timeout)
    }

    /// Steps one instruction and then enters halted state again.
    #[tracing::instrument(skip(self))]
    pub fn step(&mut self) -> Result<CoreInformation, Error> {
        self.inner.step()
    }

    /// Returns the current status of the core.
    #[tracing::instrument(level = "trace", skip(self))]
    pub fn status(&mut self) -> Result<CoreStatus, Error> {
        self.inner.status()
    }

    /// Read the value of a core register.
    ///
    /// # Remarks
    ///
    /// `T` can be an unsigned integer type, such as [u32] or [u64], or
    /// it can be [RegisterValue] to allow the caller to support arbitrary
    /// length registers.
    ///
    /// To add support to convert to a custom type implement [`TryInto<CustomType>`]
    /// for [RegisterValue].
    ///
    /// # Errors
    ///
    /// If `T` isn't large enough to hold the register value an error will be raised.
    #[tracing::instrument(skip(self, address), fields(address))]
    pub fn read_core_reg<T>(
        &mut self,
        address: impl Into<registers::RegisterId>,
    ) -> Result<T, Error>
    where
        registers::RegisterValue: TryInto<T>,
        Result<T, <registers::RegisterValue as TryInto<T>>::Error>: RegisterValueResultExt<T>,
    {
        let address = address.into();

        tracing::Span::current().record("address", format!("{address:?}"));

        let value = self.inner.read_core_reg(address)?;

        value.try_into().into_crate_error()
    }

    /// Write the value of a core register.
    ///
    /// # Errors
    ///
    /// If T is too large to write to the target register an error will be raised.
    #[tracing::instrument(skip(self, address, value))]
    pub fn write_core_reg<T>(
        &mut self,
        address: impl Into<registers::RegisterId>,
        value: T,
    ) -> Result<(), Error>
    where
        T: Into<registers::RegisterValue>,
    {
        let address = address.into();

        self.inner.write_core_reg(address, value.into())
    }

    /// Returns all the available breakpoint units of the core.
    pub fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        self.inner.available_breakpoint_units()
    }

    /// Enables breakpoints on this core. If a breakpoint is set, it will halt as soon as it is hit.
    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.inner.enable_breakpoints(state)
    }

    /// Configure the debug module to ensure software breakpoints will enter Debug Mode.
    #[tracing::instrument(skip(self))]
    pub fn debug_on_sw_breakpoint(&mut self, enabled: bool) -> Result<(), Error> {
        self.inner.debug_on_sw_breakpoint(enabled)
    }

    /// Returns a list of all the registers of this core.
    pub fn registers(&self) -> &'static registers::CoreRegisters {
        self.inner.registers()
    }

    /// Returns the program counter register.
    pub fn program_counter(&self) -> &'static CoreRegister {
        self.inner.program_counter()
    }

    /// Returns the stack pointer register.
    pub fn frame_pointer(&self) -> &'static CoreRegister {
        self.inner.frame_pointer()
    }

    /// Returns the frame pointer register.
    pub fn stack_pointer(&self) -> &'static CoreRegister {
        self.inner.stack_pointer()
    }

    /// Returns the return address register, a.k.a. link register.
    pub fn return_address(&self) -> &'static CoreRegister {
        self.inner.return_address()
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to set a hardware breakpoint at `address`.
    ///
    /// The amount of hardware breakpoints which are supported is chip specific,
    /// and can be queried using the `get_available_breakpoint_units` function.
    #[tracing::instrument(skip(self))]
    pub fn set_hw_breakpoint(&mut self, address: u64) -> Result<(), Error> {
        if !self.inner.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        // If there is a breakpoint set already, return its bp_unit_index, else find the next free index.
        let breakpoints = self.inner.hw_breakpoints()?;
        let breakpoint_comparator_index =
            match breakpoints.iter().position(|&bp| bp == Some(address)) {
                Some(breakpoint_comparator_index) => breakpoint_comparator_index,
                None => breakpoints
                    .iter()
                    .position(|bp| bp.is_none())
                    .ok_or_else(|| Error::Other("No available hardware breakpoints".to_string()))?,
            };

        tracing::debug!(
            "Trying to set HW breakpoint #{} with comparator address  {:#08x}",
            breakpoint_comparator_index,
            address
        );

        // Actually set the breakpoint. Even if it has been set, set it again so it will be active.
        self.inner
            .set_hw_breakpoint(breakpoint_comparator_index, address)
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to set a given hardware breakpoint unit to `address`.
    ///
    /// The amount of hardware breakpoints which are supported is chip specific,
    /// and can be queried using the `get_available_breakpoint_units` function.
    #[tracing::instrument(skip(self))]
    pub fn set_hw_breakpoint_unit(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        if !self.inner.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        tracing::debug!(
            "Trying to set HW breakpoint #{} with comparator address  {:#08x}",
            unit_index,
            addr
        );

        self.inner.set_hw_breakpoint(unit_index, addr)
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to clear a hardware breakpoint at `address` if there exists a breakpoint at that address.
    #[tracing::instrument(skip(self))]
    pub fn clear_hw_breakpoint(&mut self, address: u64) -> Result<(), Error> {
        let bp_position = self
            .inner
            .hw_breakpoints()?
            .iter()
            .position(|bp| *bp == Some(address));

        tracing::debug!(
            "Will clear HW breakpoint    #{} with comparator address    {:#08x}",
            bp_position.unwrap_or(usize::MAX),
            address
        );

        match bp_position {
            Some(bp_position) => {
                self.inner.clear_hw_breakpoint(bp_position)?;
                Ok(())
            }
            None => Err(Error::Other(format!(
                "No breakpoint found at address {:#010x}",
                address
            ))),
        }
    }

    /// Clear all hardware breakpoints
    ///
    /// This function will clear all HW breakpoints which are configured on the target,
    /// regardless if they are set by probe-rs, AND regardless if they are enabled or not.
    /// Also used as a helper function in [`Session::drop`](crate::session::Session).
    #[tracing::instrument(skip(self))]
    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), Error> {
        for breakpoint in (self.inner.hw_breakpoints()?).into_iter().flatten() {
            self.clear_hw_breakpoint(breakpoint)?
        }
        Ok(())
    }

    /// Returns the architecture of the core.
    pub fn architecture(&self) -> Architecture {
        self.inner.architecture()
    }

    /// Returns the core type of the core
    pub fn core_type(&self) -> CoreType {
        self.inner.core_type()
    }

    /// Returns the endianness of the current operating mode
    /// of the core.
    pub fn endianness(&mut self) -> Result<Endian, Error> {
        self.inner.endianness()
    }

    /// Determine the instruction set the core is operating in
    /// This must be queried while halted as this is a runtime
    /// decision for some core types
    pub fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        self.inner.instruction_set()
    }

    /// Determine if an FPU is present.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    pub fn fpu_support(&mut self) -> Result<bool, Error> {
        self.inner.fpu_support()
    }

    /// Determine the number of floating point registers.
    /// This must be queried while halted as this is a runtime decision for some core types.
    pub fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        self.inner.floating_point_register_count()
    }

    pub(crate) fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.inner.reset_catch_set()
    }

    pub(crate) fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.inner.reset_catch_clear()
    }

    pub(crate) fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.inner.debug_core_stop()
    }

    /// Enables vector catching for the given `condition`
    pub fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        self.inner.enable_vector_catch(condition)
    }

    /// Disables vector catching for the given `condition`
    pub fn disable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        self.inner.disable_vector_catch(condition)
    }

    /// Check if the integer size is 64-bit
    pub fn is_64_bit(&self) -> bool {
        self.inner.is_64_bit()
    }
}

impl CoreInterface for Core<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.wait_for_core_halted(timeout)
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        self.core_halted()
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        self.status()
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.halt(timeout)
    }

    fn run(&mut self) -> Result<(), Error> {
        self.run()
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.reset()
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.reset_and_halt(timeout)
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.step()
    }

    fn read_core_reg(
        &mut self,
        address: registers::RegisterId,
    ) -> Result<registers::RegisterValue, Error> {
        self.read_core_reg(address)
    }

    fn write_core_reg(
        &mut self,
        address: registers::RegisterId,
        value: registers::RegisterValue,
    ) -> Result<(), Error> {
        self.write_core_reg(address, value)
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        self.available_breakpoint_units()
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        self.inner.hw_breakpoints()
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.enable_breakpoints(state)
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.set_hw_breakpoint_unit(unit_index, addr)
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.inner.clear_hw_breakpoint(unit_index)?;
        Ok(())
    }

    fn registers(&self) -> &'static registers::CoreRegisters {
        self.registers()
    }

    fn program_counter(&self) -> &'static CoreRegister {
        self.program_counter()
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        self.frame_pointer()
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        self.stack_pointer()
    }

    fn return_address(&self) -> &'static CoreRegister {
        self.return_address()
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.inner.hw_breakpoints_enabled()
    }

    fn architecture(&self) -> Architecture {
        self.architecture()
    }

    fn core_type(&self) -> CoreType {
        self.core_type()
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        self.instruction_set()
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        self.fpu_support()
    }

    fn floating_point_register_count(&mut self) -> Result<usize, crate::error::Error> {
        self.floating_point_register_count()
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.reset_catch_set()
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.reset_catch_clear()
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.debug_core_stop()
    }

    fn is_64_bit(&self) -> bool {
        self.is_64_bit()
    }
}

pub enum ResolvedCoreOptions {
    Arm {
        sequence: Arc<dyn ArmDebugSequence>,
        options: ArmCoreAccessOptions,
    },
    Riscv {
        sequence: Arc<dyn RiscvDebugSequence>,
        options: RiscvCoreAccessOptions,
    },
    Xtensa {
        sequence: Arc<dyn XtensaDebugSequence>,
        options: XtensaCoreAccessOptions,
    },
}

impl ResolvedCoreOptions {
    fn new(target: &Target, options: CoreAccessOptions) -> Self {
        match (options, target.debug_sequence.clone()) {
            (CoreAccessOptions::Arm(options), DebugSequence::Arm(sequence)) => {
                Self::Arm { sequence, options }
            }
            (CoreAccessOptions::Riscv(options), DebugSequence::Riscv(sequence)) => {
                Self::Riscv { sequence, options }
            }
            (CoreAccessOptions::Xtensa(options), DebugSequence::Xtensa(sequence)) => {
                Self::Xtensa { sequence, options }
            }
            _ => unreachable!(
                "Mismatch between core kind and access options. This is a bug, please report it."
            ),
        }
    }

    fn interface_idx(&self) -> usize {
        match self {
            Self::Arm { options, .. } => options.jtag_tap.unwrap_or(0),
            Self::Riscv { options, .. } => options.jtag_tap.unwrap_or(0),
            Self::Xtensa { options, .. } => options.jtag_tap.unwrap_or(0),
        }
    }
}

impl std::fmt::Debug for ResolvedCoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arm { options, .. } => f
                .debug_struct("Arm")
                .field("sequence", &"<ArmDebugSequence>")
                .field("options", options)
                .finish(),
            Self::Riscv { options, .. } => f
                .debug_struct("Riscv")
                .field("sequence", &"<RiscvDebugSequence>")
                .field("options", options)
                .finish(),
            Self::Xtensa { options, .. } => f
                .debug_struct("Xtensa")
                .field("sequence", &"<XtensaDebugSequence>")
                .field("options", options)
                .finish(),
        }
    }
}
