use crate::{
    architecture::{
        arm::{
            core::registers::{
                aarch32::{
                    AARCH32_CORE_REGSISTERS, AARCH32_WITH_FP_16_CORE_REGSISTERS,
                    AARCH32_WITH_FP_32_CORE_REGSISTERS,
                },
                aarch64::AARCH64_CORE_REGSISTERS,
                cortex_m::{CORTEX_M_CORE_REGISTERS, CORTEX_M_WITH_FP_CORE_REGISTERS},
            },
            sequences::ArmDebugSequence,
        },
        riscv::registers::RISCV_CORE_REGSISTERS,
        xtensa::registers::XTENSA_CORE_REGSISTERS,
    },
    config::DebugSequence,
    debug::{DebugRegister, DebugRegisters},
    error, CoreType, Error, InstructionSet, MemoryInterface, Target,
};
use anyhow::anyhow;
pub use probe_rs_target::{Architecture, CoreAccessOptions};
use probe_rs_target::{
    ArmCoreAccessOptions, MemoryRange, MemoryRegion, RiscvCoreAccessOptions,
    XtensaCoreAccessOptions,
};
use scroll::Pread;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    mem::size_of_val,
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

pub mod core_state;
pub mod core_status;
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
    /// a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) error will be returned.
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error>;

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) error will be returned.
    fn core_halted(&mut self) -> Result<bool, error::Error>;

    /// Returns the current status of the core.
    fn status(&mut self) -> Result<CoreStatus, error::Error>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) otherwise.
    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error>;

    /// Continue to execute instructions.
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

    /// Read the value of a core register.
    fn read_core_reg(
        &mut self,
        address: registers::RegisterId,
    ) -> Result<registers::RegisterValue, error::Error>;

    /// Write the value of a core register.
    fn write_core_reg(
        &mut self,
        address: registers::RegisterId,
        value: registers::RegisterValue,
    ) -> Result<(), error::Error>;

    /// Returns all the available breakpoint units of the core.
    fn available_breakpoint_units(&mut self) -> Result<u32, error::Error>;

    /// Read the hardware breakpoints from FpComp registers, and adds them to the Result Vector.
    /// A value of None in any position of the Vector indicates that the position is unset/available.
    /// We intentionally return all breakpoints, irrespective of whether they are enabled or not.
    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, error::Error>;

    /// Enables breakpoints on this core. If a breakpoint is set, it will halt as soon as it is hit.
    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error>;

    /// Sets a breakpoint at `addr`. It does so by using unit `bp_unit_index`.
    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), error::Error>;

    /// Clears the breakpoint configured in unit `unit_index`.
    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error>;

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
    fn debug_on_sw_breakpoint(&mut self, _enabled: bool) -> Result<(), error::Error> {
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
    fn instruction_set(&mut self) -> Result<InstructionSet, error::Error>;

    /// Determine if an FPU is present.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    fn fpu_support(&mut self) -> Result<bool, error::Error>;

    /// Determine the number of floating point registers.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    fn floating_point_register_count(&mut self) -> Result<usize, crate::error::Error>;

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
}

/// A snapshot representation of a core state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreDump {
    /// The registers we dumped from the core.
    pub registers: HashMap<RegisterId, RegisterValue>,
    /// The memory we dumped from the core.
    pub data: Vec<(Range<u64>, Vec<u8>)>,
    /// The instruction set of the dumped core.
    pub instruction_set: InstructionSet,
    /// Whether or not the target supports native 64 bit support (64bit architectures)
    pub supports_native_64bit_access: bool,
    /// The type of core we have at hand.
    pub core_type: CoreType,
    /// Whether this core supports floating point.
    pub fpu_support: bool,
    /// The number of floating point registers.
    pub floating_point_register_count: Option<usize>,
}

impl CoreDump {
    /// Store the dumped core to a file.
    pub fn store(&self, path: &Path) -> Result<(), CoreDumpError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(path)
            .map_err(|e| {
                CoreDumpError::CoreDumpFileWrite(e, dunce::canonicalize(path).unwrap_or_default())
            })?;
        rmp_serde::encode::write_named(&mut file, self).map_err(CoreDumpError::EncodingCoreDump)?;
        Ok(())
    }

    /// Load the dumped core from a file.
    pub fn load(path: &Path) -> Result<Self, CoreDumpError> {
        let file = OpenOptions::new().read(true).open(path).map_err(|e| {
            CoreDumpError::CoreDumpFileRead(e, dunce::canonicalize(path).unwrap_or_default())
        })?;
        rmp_serde::from_read(&file).map_err(CoreDumpError::DecodingCoreDump)
    }

    /// Load the dumped core from a file.
    pub fn load_raw(data: &[u8]) -> Result<Self, CoreDumpError> {
        rmp_serde::from_slice(data).map_err(CoreDumpError::DecodingCoreDump)
    }

    /// Read all registers defined in [`crate::core::CoreRegisters`] from the given core.
    pub fn debug_registers(&self) -> DebugRegisters {
        let reg_list = match self.core_type {
            CoreType::Armv6m => &CORTEX_M_CORE_REGISTERS,
            CoreType::Armv7a => match self.floating_point_register_count {
                Some(16) => &AARCH32_WITH_FP_16_CORE_REGSISTERS,
                Some(32) => &AARCH32_WITH_FP_32_CORE_REGSISTERS,
                _ => &AARCH32_CORE_REGSISTERS,
            },
            CoreType::Armv7m => {
                if self.fpu_support {
                    &CORTEX_M_WITH_FP_CORE_REGISTERS
                } else {
                    &CORTEX_M_CORE_REGISTERS
                }
            }
            CoreType::Armv7em => {
                if self.fpu_support {
                    &CORTEX_M_WITH_FP_CORE_REGISTERS
                } else {
                    &CORTEX_M_CORE_REGISTERS
                }
            }
            // TODO: This can be wrong if the CPU is 32 bit. For lack of better design at the time
            // of writing this code this differentiation has been omitted.
            CoreType::Armv8a => &AARCH64_CORE_REGSISTERS,
            CoreType::Armv8m => {
                if self.fpu_support {
                    &CORTEX_M_WITH_FP_CORE_REGISTERS
                } else {
                    &CORTEX_M_CORE_REGISTERS
                }
            }
            CoreType::Riscv => &RISCV_CORE_REGSISTERS,
            CoreType::Xtensa => &XTENSA_CORE_REGSISTERS,
        };

        let mut debug_registers = Vec::<DebugRegister>::new();
        for (dwarf_id, core_register) in reg_list.core_registers().enumerate() {
            // Check to ensure the register type is compatible with u64.
            if matches!(core_register.data_type(), RegisterDataType::UnsignedInteger(size_in_bits) if size_in_bits <= 64)
            {
                debug_registers.push(DebugRegister {
                    core_register,
                    // The DWARF register ID is only valid for the first 32 registers.
                    dwarf_id: if dwarf_id < 32 {
                        Some(dwarf_id as u16)
                    } else {
                        None
                    },
                    value: match self.registers.get(&core_register.id()) {
                        Some(register_value) => Some(*register_value),
                        None => {
                            tracing::warn!("Failed to read value for register {:?}", core_register);
                            None
                        }
                    },
                });
            } else {
                tracing::trace!(
                    "Unwind will use the default rule for this register : {:?}",
                    core_register
                );
            }
        }
        DebugRegisters(debug_registers)
    }

    /// Returns the type of the core.
    pub fn core_type(&self) -> CoreType {
        self.core_type
    }

    /// Returns the currently active instruction-set
    pub fn instruction_set(&self) -> InstructionSet {
        self.instruction_set
    }

    /// Retrieve a memory range that contains the requested address and size, from the coredump.
    fn get_memory_from_coredump(
        &self,
        address: u64,
        size_in_bytes: u64,
    ) -> Result<(u64, &Vec<u8>), crate::Error> {
        for (range, memory) in &self.data {
            if range.contains_range(&(address..(address + size_in_bytes))) {
                return Ok((range.start, memory));
            }
        }
        // If we get here, then no range with the requested memory address and size was found.
        Err(crate::Error::Other(anyhow!("The coredump does not include the memory for address {address:#x} of size {size_in_bytes:#x}")))
    }

    /// Read the requested memory range from the coredump, and return the data in the requested buffer.
    /// The word-size of the read is determined by the size of the items in the `data` buffer.
    fn read_memory_range<'a, T>(
        &'a self,
        address: u64,
        data: &'a mut [T],
    ) -> Result<(), crate::Error>
    where
        <T as scroll::ctx::TryFromCtx<'a, scroll::Endian>>::Error:
            std::convert::From<scroll::Error>,
        <T as scroll::ctx::TryFromCtx<'a, scroll::Endian>>::Error: std::fmt::Display,
        T: scroll::ctx::TryFromCtx<'a, scroll::Endian>,
    {
        let (memory_offset, memory) =
            self.get_memory_from_coredump(address, (size_of_val(data)) as u64)?;
        for (n, data) in data.iter_mut().enumerate() {
            *data = memory
                .pread_with::<T>((address - memory_offset) as usize + n * 4, scroll::LE)
                .map_err(|e| anyhow!("{e}"))?;
        }
        Ok(())
    }
}

impl MemoryInterface for CoreDump {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.supports_native_64bit_access
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, crate::Error> {
        let mut data = [0u64; 1];
        self.read_memory_range(address, &mut data)?;
        Ok(data[0])
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, crate::Error> {
        let mut data = [0u32; 1];
        self.read_memory_range(address, &mut data)?;
        Ok(data[0])
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, crate::Error> {
        let mut data = [0u16; 1];
        self.read_memory_range(address, &mut data)?;
        Ok(data[0])
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, crate::Error> {
        let mut data = [0u8; 1];
        self.read_memory_range(address, &mut data)?;
        Ok(data[0])
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), crate::Error> {
        self.read_memory_range(address, data)?;
        Ok(())
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), crate::Error> {
        self.read_memory_range(address, data)?;
        Ok(())
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), crate::Error> {
        self.read_memory_range(address, data)?;
        Ok(())
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), crate::Error> {
        self.read_memory_range(address, data)?;
        Ok(())
    }

    fn write_word_64(&mut self, _address: u64, _data: u64) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_32(&mut self, _address: u64, _data: u32) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_16(&mut self, _address: u64, _data: u16) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_word_8(&mut self, _address: u64, _data: u8) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_64(&mut self, _address: u64, _data: &[u64]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_32(&mut self, _address: u64, _data: &[u32]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_16(&mut self, _address: u64, _data: &[u16]) -> Result<(), crate::Error> {
        todo!()
    }

    fn write_8(&mut self, _address: u64, _data: &[u8]) -> Result<(), crate::Error> {
        todo!()
    }

    fn supports_8bit_transfers(&self) -> Result<bool, crate::Error> {
        todo!()
    }

    fn flush(&mut self) -> Result<(), crate::Error> {
        todo!()
    }
}

/// The overarching error type which contains all possible errors as variants.
#[derive(thiserror::Error, Debug)]
pub enum CoreDumpError {
    /// Opening the file for writing the core dump failed.
    #[error("Opening {1} for writing the core dump failed.")]
    CoreDumpFileWrite(std::io::Error, PathBuf),
    /// Opening the file for reading the core dump failed.
    #[error("Opening {1} for reading the core dump failed.")]
    CoreDumpFileRead(std::io::Error, PathBuf),
    /// Encoding the coredump MessagePack failed.
    #[error("Encoding the coredump MessagePack failed.")]
    EncodingCoreDump(rmp_serde::encode::Error),
    /// Decoding the coredump MessagePack failed.
    #[error("Decoding the coredump MessagePack failed.")]
    DecodingCoreDump(rmp_serde::decode::Error),
}

impl<'probe> MemoryInterface for Core<'probe> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.inner.supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.inner.read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.inner.read_word_32(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.inner.read_word_16(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.inner.read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.inner.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.inner.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.inner.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.inner.read_8(address, data)
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.inner.read(address, data)
    }

    fn write_word_64(&mut self, addr: u64, data: u64) -> Result<(), Error> {
        self.inner.write_word_64(addr, data)
    }

    fn write_word_32(&mut self, addr: u64, data: u32) -> Result<(), Error> {
        self.inner.write_word_32(addr, data)
    }

    fn write_word_16(&mut self, addr: u64, data: u16) -> Result<(), Error> {
        self.inner.write_word_16(addr, data)
    }

    fn write_word_8(&mut self, addr: u64, data: u8) -> Result<(), Error> {
        self.inner.write_word_8(addr, data)
    }

    fn write_64(&mut self, addr: u64, data: &[u64]) -> Result<(), Error> {
        self.inner.write_64(addr, data)
    }

    fn write_32(&mut self, addr: u64, data: &[u32]) -> Result<(), Error> {
        self.inner.write_32(addr, data)
    }

    fn write_16(&mut self, addr: u64, data: &[u16]) -> Result<(), Error> {
        self.inner.write_16(addr, data)
    }

    fn write_8(&mut self, addr: u64, data: &[u8]) -> Result<(), Error> {
        self.inner.write_8(addr, data)
    }

    fn write(&mut self, addr: u64, data: &[u8]) -> Result<(), Error> {
        self.inner.write(addr, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, error::Error> {
        self.inner.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.inner.flush()
    }
}

/// A struct containing key information about an exception.
/// The exception details are architecture specific, and the abstraction is handled in the
/// architecture specific implementations of [`crate::core::ExceptionInterface`].
#[derive(Debug, PartialEq)]
pub struct ExceptionInfo {
    /// A human readable explanation for the exception.
    pub description: String,
    /// The stackframe registers, and their values, for the frame that triggered the exception.
    pub calling_frame_registers: DebugRegisters,
}

/// A generic interface to identify and decode exceptions during unwind processing.
pub trait ExceptionInterface {
    /// Using the `stackframe_registers` for a "called frame",
    /// determine if the given frame was called from an exception handler,
    /// and resolve the relevant details about the exception, including the reason for the exception,
    /// and the stackframe registers for the frame that triggered the exception.
    /// A return value of `Ok(None)` indicates that the given frame was called from within the current thread,
    /// and the unwind should continue normally.
    fn exception_details(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error>;

    /// Using the `stackframe_registers` for a "called frame", retrieve updated register values for the "calling frame".
    fn calling_frame_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error>;

    /// Convert the architecture specific exception number into a human readable description.
    /// Where possible, the implementation may read additional registers from the core, to provide additional context.
    fn exception_description(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error>;
}

/// Placeholder for exception handling for cores where handling exceptions is not yet supported.
pub struct UnimplementedExceptionHandler;

impl ExceptionInterface for UnimplementedExceptionHandler {
    fn exception_details(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &DebugRegisters,
    ) -> Result<Option<ExceptionInfo>, Error> {
        // For architectures where the exception handling has not been implemented in probe-rs,
        // this will result in maintaining the current `unwind` behavior, i.e. unwinding will stop
        // when the first frame is reached that was called from an exception handler.
        Err(Error::NotImplemented(
            "Unwinding of exception frames has not yet been implemented for this architecture.",
        ))
    }

    fn calling_frame_registers(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        Err(Error::NotImplemented(
            "Not implemented for this architecture.",
        ))
    }

    fn exception_description(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<String, crate::Error> {
        Err(Error::NotImplemented(
            "Not implemented for this architecture.",
        ))
    }
}

/// Creates a new exception interface for the [`CoreType`] at hand.
pub fn exception_handler_for_core(core_type: CoreType) -> Box<dyn ExceptionInterface> {
    match core_type {
        CoreType::Armv6m => {
            Box::new(crate::architecture::arm::core::exception_handling::ArmV6MExceptionHandler {})
        }
        CoreType::Armv7m | CoreType::Armv7em => {
            Box::new(crate::architecture::arm::core::exception_handling::ArmV7MExceptionHandler {})
        }
        CoreType::Armv8m => Box::new(
            crate::architecture::arm::core::exception_handling::armv8m::ArmV8MExceptionHandler,
        ),
        CoreType::Armv7a | CoreType::Armv8a | CoreType::Riscv | CoreType::Xtensa => {
            Box::new(UnimplementedExceptionHandler)
        }
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
    memory_regions: &'probe [MemoryRegion],

    inner: Box<dyn CoreInterface + 'probe>,
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
        memory_regions: &'probe [MemoryRegion],
        core: impl CoreInterface + 'probe,
    ) -> Core<'probe> {
        Self {
            id,
            name,
            memory_regions,
            inner: Box::new(core),
        }
    }

    /// Return the memory regions associated with this core.
    pub fn memory_regions(&self) -> impl Iterator<Item = &MemoryRegion> {
        self.memory_regions
            .iter()
            .filter(|r| r.cores().iter().any(|m| m == self.name))
    }

    /// Creates a new [`CoreState`]
    pub(crate) fn create_state(
        id: usize,
        options: CoreAccessOptions,
        target: &Target,
        core_type: CoreType,
    ) -> CombinedCoreState {
        let specific_state = SpecificCoreState::from_core_type(core_type);

        match options {
            CoreAccessOptions::Arm(options) => {
                let DebugSequence::Arm(sequence) = target.debug_sequence.clone() else {
                    panic!(
                        "Mismatch between sequence and core kind. This is a bug, please report it."
                    );
                };

                let core_state = CoreState::new(ResolvedCoreOptions::Arm { sequence, options });

                CombinedCoreState {
                    id,
                    core_state,
                    specific_state,
                }
            }
            CoreAccessOptions::Riscv(options) => {
                let core_state = CoreState::new(ResolvedCoreOptions::Riscv { options });
                CombinedCoreState {
                    id,
                    core_state,
                    specific_state,
                }
            }
            CoreAccessOptions::Xtensa(options) => {
                let core_state = CoreState::new(ResolvedCoreOptions::Xtensa { options });
                CombinedCoreState {
                    id,
                    core_state,
                    specific_state,
                }
            }
        }
    }

    /// Returns the ID of this core.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) error will be returned.
    #[tracing::instrument(skip(self))]
    pub fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        self.inner.wait_for_core_halted(timeout)
    }

    /// Check if the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) error will be returned.
    pub fn core_halted(&mut self) -> Result<bool, error::Error> {
        self.inner.core_halted()
    }

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`DebugProbeError::Timeout`](crate::DebugProbeError::Timeout) otherwise.
    #[tracing::instrument(skip(self))]
    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.inner.halt(timeout)
    }

    /// Continue to execute instructions.
    #[tracing::instrument(skip(self))]
    pub fn run(&mut self) -> Result<(), error::Error> {
        self.inner.run()
    }

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: Core::reset_and_halt
    #[tracing::instrument(skip(self))]
    pub fn reset(&mut self) -> Result<(), error::Error> {
        self.inner.reset()
    }

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: Core::reset
    #[tracing::instrument(skip(self))]
    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.inner.reset_and_halt(timeout)
    }

    /// Steps one instruction and then enters halted state again.
    #[tracing::instrument(skip(self))]
    pub fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.inner.step()
    }

    /// Returns the current status of the core.
    #[tracing::instrument(skip(self))]
    pub fn status(&mut self) -> Result<CoreStatus, error::Error> {
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
    /// for [RegisterValue]].
    ///
    /// # Errors
    ///
    /// If `T` isn't large enough to hold the register value an error will be raised.
    #[tracing::instrument(skip(self, address), fields(address))]
    pub fn read_core_reg<T>(
        &mut self,
        address: impl Into<registers::RegisterId>,
    ) -> Result<T, error::Error>
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
    ) -> Result<(), error::Error>
    where
        T: Into<registers::RegisterValue>,
    {
        let address = address.into();

        self.inner.write_core_reg(address, value.into())
    }

    /// Returns all the available breakpoint units of the core.
    pub fn available_breakpoint_units(&mut self) -> Result<u32, error::Error> {
        self.inner.available_breakpoint_units()
    }

    /// Enables breakpoints on this core. If a breakpoint is set, it will halt as soon as it is hit.
    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error> {
        self.inner.enable_breakpoints(state)
    }

    /// Configure the debug module to ensure software breakpoints will enter Debug Mode.
    #[tracing::instrument(skip(self))]
    pub fn debug_on_sw_breakpoint(&mut self, enabled: bool) -> Result<(), error::Error> {
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

    /// Find the index of the next available HW breakpoint comparator.
    fn find_free_breakpoint_comparator_index(&mut self) -> Result<usize, error::Error> {
        let mut next_available_hw_breakpoint = 0;
        for breakpoint in self.inner.hw_breakpoints()? {
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
    /// This function will try to set a hardware breakpoint att `address`.
    ///
    /// The amount of hardware breakpoints which are supported is chip specific,
    /// and can be queried using the `get_available_breakpoint_units` function.
    #[tracing::instrument(skip(self))]
    pub fn set_hw_breakpoint(&mut self, address: u64) -> Result<(), error::Error> {
        if !self.inner.hw_breakpoints_enabled() {
            self.enable_breakpoints(true)?;
        }

        // If there is a breakpoint set already, return its bp_unit_index, else find the next free index.
        let breakpoint_comparator_index = match self
            .inner
            .hw_breakpoints()?
            .iter()
            .position(|&bp| bp == Some(address))
        {
            Some(breakpoint_comparator_index) => breakpoint_comparator_index,
            None => self.find_free_breakpoint_comparator_index()?,
        };

        tracing::debug!(
            "Trying to set HW breakpoint #{} with comparator address  {:#08x}",
            breakpoint_comparator_index,
            address
        );

        // Actually set the breakpoint. Even if it has been set, set it again so it will be active.
        self.inner
            .set_hw_breakpoint(breakpoint_comparator_index, address)?;
        Ok(())
    }

    /// Set a hardware breakpoint
    ///
    /// This function will try to clear a hardware breakpoint at `address` if there exists a breakpoint at that address.
    #[tracing::instrument(skip(self))]
    pub fn clear_hw_breakpoint(&mut self, address: u64) -> Result<(), error::Error> {
        let bp_position = self
            .inner
            .hw_breakpoints()?
            .iter()
            .position(|bp| bp.is_some() && bp.unwrap() == address);

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
            None => Err(error::Error::Other(anyhow!(
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
    pub fn clear_all_hw_breakpoints(&mut self) -> Result<(), error::Error> {
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

    /// Determine the instruction set the core is operating in
    /// This must be queried while halted as this is a runtime
    /// decision for some core types
    pub fn instruction_set(&mut self) -> Result<InstructionSet, error::Error> {
        self.inner.instruction_set()
    }

    /// Determine if an FPU is present.
    /// This must be queried while halted as this is a runtime
    /// decision for some core types.
    pub fn fpu_support(&mut self) -> Result<bool, error::Error> {
        self.inner.fpu_support()
    }

    /// Determine the number of floating point registers.
    /// This must be queried while halted as this is a runtime decision for some core types.
    pub fn floating_point_register_count(&mut self) -> Result<usize, error::Error> {
        self.inner.floating_point_register_count()
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

    /// Dumps core info with the current state.
    ///
    /// # Arguments
    ///
    /// * `ranges`: Memory ranges that should be dumped.
    pub fn dump(&mut self, ranges: Vec<Range<u64>>) -> Result<CoreDump, Error> {
        let instruction_set = self.instruction_set()?;
        let core_type = self.core_type();
        let supports_native_64bit_access = self.supports_native_64bit_access();
        let fpu_support = self.fpu_support()?;
        let floating_point_register_count = self.floating_point_register_count()?;

        let mut registers = HashMap::new();
        for register in self.registers().all_registers() {
            let value = self.read_core_reg(register.id())?;
            registers.insert(register.id(), value);
        }

        let mut data = Vec::new();
        for range in ranges {
            let mut values = vec![0; (range.end - range.start) as usize];
            self.read_8(range.start, &mut values)?;
            data.push((range, values));
        }

        Ok(CoreDump {
            registers,
            data,
            instruction_set,
            supports_native_64bit_access,
            core_type,
            fpu_support,
            floating_point_register_count: Some(floating_point_register_count),
        })
    }
}

impl<'probe> CoreInterface for Core<'probe> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), error::Error> {
        self.wait_for_core_halted(timeout)
    }

    fn core_halted(&mut self) -> Result<bool, error::Error> {
        self.core_halted()
    }

    fn status(&mut self) -> Result<CoreStatus, error::Error> {
        self.status()
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.halt(timeout)
    }

    fn run(&mut self) -> Result<(), error::Error> {
        self.run()
    }

    fn reset(&mut self) -> Result<(), error::Error> {
        self.reset()
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.reset_and_halt(timeout)
    }

    fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.step()
    }

    fn read_core_reg(
        &mut self,
        address: registers::RegisterId,
    ) -> Result<registers::RegisterValue, error::Error> {
        self.read_core_reg(address)
    }

    fn write_core_reg(
        &mut self,
        address: registers::RegisterId,
        value: registers::RegisterValue,
    ) -> Result<(), error::Error> {
        self.write_core_reg(address, value)
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, error::Error> {
        self.available_breakpoint_units()
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, error::Error> {
        todo!()
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), error::Error> {
        self.enable_breakpoints(state)
    }

    fn set_hw_breakpoint(&mut self, _unit_index: usize, addr: u64) -> Result<(), error::Error> {
        self.set_hw_breakpoint(addr)
    }

    fn clear_hw_breakpoint(&mut self, _unit_index: usize) -> Result<(), error::Error> {
        self.clear_all_hw_breakpoints()
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
        todo!()
    }

    fn architecture(&self) -> Architecture {
        self.architecture()
    }

    fn core_type(&self) -> CoreType {
        self.core_type()
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, error::Error> {
        self.instruction_set()
    }

    fn fpu_support(&mut self) -> Result<bool, error::Error> {
        self.fpu_support()
    }

    fn floating_point_register_count(&mut self) -> Result<usize, crate::error::Error> {
        self.floating_point_register_count()
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        todo!()
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.reset_catch_clear()
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.debug_core_stop()
    }
}

pub enum ResolvedCoreOptions {
    Arm {
        sequence: Arc<dyn ArmDebugSequence>,
        options: ArmCoreAccessOptions,
    },
    Riscv {
        options: RiscvCoreAccessOptions,
    },
    Xtensa {
        options: XtensaCoreAccessOptions,
    },
}

impl std::fmt::Debug for ResolvedCoreOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Arm { options, .. } => f
                .debug_struct("Arm")
                .field("sequence", &"<ArmDebugSequence>")
                .field("options", options)
                .finish(),
            Self::Riscv { options } => f.debug_struct("Riscv").field("options", options).finish(),
            Self::Xtensa { options } => f.debug_struct("Xtensa").field("options", options).finish(),
        }
    }
}
