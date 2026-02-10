//! ARMv7-A core interface (Cortex-A7, A9, A15, etc.)

use super::CortexARState;
use super::armv7ar::Armv7ar;
use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegister, CoreStatus, CoreType, Endian,
    Error, InstructionSet, MemoryInterface, RegisterId, RegisterValue, VectorCatchCondition,
    architecture::arm::{memory::ArmMemoryInterface, sequences::ArmDebugSequence},
    core::CoreRegisters,
};
use std::{sync::Arc, time::Duration};

/// Interface for interacting with an ARMv7-A core (Cortex-A series)
pub struct Armv7a<'probe>(pub(crate) Armv7ar<'probe>);

impl<'probe> Armv7a<'probe> {
    pub(crate) fn new(
        memory: Box<dyn ArmMemoryInterface + 'probe>,
        state: &'probe mut CortexARState,
        base_address: u64,
        sequence: Arc<dyn ArmDebugSequence>,
    ) -> Result<Self, Error> {
        Ok(Self(Armv7ar::new(
            memory,
            state,
            base_address,
            sequence,
            CoreType::Armv7a,
        )?))
    }
}

// Implement CoreInterface by delegating to the inner Armv7ar
impl CoreInterface for Armv7a<'_> {
    fn wait_for_core_halted(&mut self, timeout: Duration) -> Result<(), Error> {
        self.0.wait_for_core_halted(timeout)
    }

    fn core_halted(&mut self) -> Result<bool, Error> {
        self.0.core_halted()
    }

    fn status(&mut self) -> Result<CoreStatus, Error> {
        self.0.status()
    }

    fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.0.halt(timeout)
    }

    fn run(&mut self) -> Result<(), Error> {
        self.0.run()
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.0.reset()
    }

    fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, Error> {
        self.0.reset_and_halt(timeout)
    }

    fn step(&mut self) -> Result<CoreInformation, Error> {
        self.0.step()
    }

    fn read_core_reg(&mut self, address: RegisterId) -> Result<RegisterValue, Error> {
        self.0.read_core_reg(address)
    }

    fn write_core_reg(&mut self, address: RegisterId, value: RegisterValue) -> Result<(), Error> {
        self.0.write_core_reg(address, value)
    }

    fn available_breakpoint_units(&mut self) -> Result<u32, Error> {
        self.0.available_breakpoint_units()
    }

    fn hw_breakpoints(&mut self) -> Result<Vec<Option<u64>>, Error> {
        self.0.hw_breakpoints()
    }

    fn enable_breakpoints(&mut self, state: bool) -> Result<(), Error> {
        self.0.enable_breakpoints(state)
    }

    fn set_hw_breakpoint(&mut self, unit_index: usize, addr: u64) -> Result<(), Error> {
        self.0.set_hw_breakpoint(unit_index, addr)
    }

    fn clear_hw_breakpoint(&mut self, unit_index: usize) -> Result<(), Error> {
        self.0.clear_hw_breakpoint(unit_index)
    }

    fn registers(&self) -> &'static CoreRegisters {
        self.0.registers()
    }

    fn program_counter(&self) -> &'static CoreRegister {
        self.0.program_counter()
    }

    fn frame_pointer(&self) -> &'static CoreRegister {
        self.0.frame_pointer()
    }

    fn stack_pointer(&self) -> &'static CoreRegister {
        self.0.stack_pointer()
    }

    fn return_address(&self) -> &'static CoreRegister {
        self.0.return_address()
    }

    fn hw_breakpoints_enabled(&self) -> bool {
        self.0.hw_breakpoints_enabled()
    }

    fn architecture(&self) -> Architecture {
        self.0.architecture()
    }

    fn core_type(&self) -> CoreType {
        self.0.core_type()
    }

    fn instruction_set(&mut self) -> Result<InstructionSet, Error> {
        self.0.instruction_set()
    }

    fn endianness(&mut self) -> Result<Endian, Error> {
        self.0.endianness()
    }

    fn fpu_support(&mut self) -> Result<bool, Error> {
        self.0.fpu_support()
    }

    fn floating_point_register_count(&mut self) -> Result<usize, Error> {
        self.0.floating_point_register_count()
    }

    fn reset_catch_set(&mut self) -> Result<(), Error> {
        self.0.reset_catch_set()
    }

    fn reset_catch_clear(&mut self) -> Result<(), Error> {
        self.0.reset_catch_clear()
    }

    fn debug_core_stop(&mut self) -> Result<(), Error> {
        self.0.debug_core_stop()
    }

    fn enable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        self.0.enable_vector_catch(condition)
    }

    fn disable_vector_catch(&mut self, condition: VectorCatchCondition) -> Result<(), Error> {
        self.0.disable_vector_catch(condition)
    }

    fn is_64_bit(&self) -> bool {
        self.0.is_64_bit()
    }

    fn spill_registers(&mut self) -> Result<(), Error> {
        self.0.spill_registers()
    }
}

// Implement MemoryInterface by delegating to the inner Armv7ar
impl MemoryInterface for Armv7a<'_> {
    fn supports_native_64bit_access(&mut self) -> bool {
        self.0.supports_native_64bit_access()
    }

    fn read_word_64(&mut self, address: u64) -> Result<u64, Error> {
        self.0.read_word_64(address)
    }

    fn read_word_32(&mut self, address: u64) -> Result<u32, Error> {
        self.0.read_word_32(address)
    }

    fn read_word_16(&mut self, address: u64) -> Result<u16, Error> {
        self.0.read_word_16(address)
    }

    fn read_word_8(&mut self, address: u64) -> Result<u8, Error> {
        self.0.read_word_8(address)
    }

    fn read_64(&mut self, address: u64, data: &mut [u64]) -> Result<(), Error> {
        self.0.read_64(address, data)
    }

    fn read_32(&mut self, address: u64, data: &mut [u32]) -> Result<(), Error> {
        self.0.read_32(address, data)
    }

    fn read_16(&mut self, address: u64, data: &mut [u16]) -> Result<(), Error> {
        self.0.read_16(address, data)
    }

    fn read_8(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.0.read_8(address, data)
    }

    fn read(&mut self, address: u64, data: &mut [u8]) -> Result<(), Error> {
        self.0.read(address, data)
    }

    fn write_word_64(&mut self, address: u64, data: u64) -> Result<(), Error> {
        self.0.write_word_64(address, data)
    }

    fn write_word_32(&mut self, address: u64, data: u32) -> Result<(), Error> {
        self.0.write_word_32(address, data)
    }

    fn write_word_16(&mut self, address: u64, data: u16) -> Result<(), Error> {
        self.0.write_word_16(address, data)
    }

    fn write_word_8(&mut self, address: u64, data: u8) -> Result<(), Error> {
        self.0.write_word_8(address, data)
    }

    fn write_64(&mut self, address: u64, data: &[u64]) -> Result<(), Error> {
        self.0.write_64(address, data)
    }

    fn write_32(&mut self, address: u64, data: &[u32]) -> Result<(), Error> {
        self.0.write_32(address, data)
    }

    fn write_16(&mut self, address: u64, data: &[u16]) -> Result<(), Error> {
        self.0.write_16(address, data)
    }

    fn write_8(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.0.write_8(address, data)
    }

    fn write(&mut self, address: u64, data: &[u8]) -> Result<(), Error> {
        self.0.write(address, data)
    }

    fn supports_8bit_transfers(&self) -> Result<bool, Error> {
        self.0.supports_8bit_transfers()
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.0.flush()
    }
}
