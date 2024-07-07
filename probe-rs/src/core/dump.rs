use crate::{
    architecture::{
        arm::core::registers::{
            aarch32::{
                AARCH32_CORE_REGSISTERS, AARCH32_WITH_FP_16_CORE_REGSISTERS,
                AARCH32_WITH_FP_32_CORE_REGSISTERS,
            },
            aarch64::AARCH64_CORE_REGSISTERS,
            cortex_m::{CORTEX_M_CORE_REGISTERS, CORTEX_M_WITH_FP_CORE_REGISTERS},
        },
        riscv::registers::RISCV_CORE_REGSISTERS,
        xtensa::registers::XTENSA_CORE_REGSISTERS,
    },
    debug::{DebugRegister, DebugRegisters},
    Core, CoreType, Error, InstructionSet, MemoryInterface,
};
use crate::{RegisterId, RegisterValue};
use anyhow::anyhow;
use probe_rs_target::MemoryRange;
use scroll::Pread;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    mem::size_of_val,
    ops::Range,
    path::{Path, PathBuf},
};

use super::RegisterDataType;

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
    /// Dump the core info with the current state.
    ///
    /// # Arguments
    /// * `core`: The core to dump.
    /// * `ranges`: Memory ranges that should be dumped.
    pub fn dump_core(core: &mut Core, ranges: Vec<Range<u64>>) -> Result<Self, Error> {
        let instruction_set = core.instruction_set()?;
        let core_type = core.core_type();
        let supports_native_64bit_access = core.supports_native_64bit_access();
        let fpu_support = core.fpu_support()?;
        let floating_point_register_count = core.floating_point_register_count()?;

        let mut registers = HashMap::new();
        for register in core.registers().all_registers() {
            let value = core.read_core_reg(register.id())?;
            registers.insert(register.id(), value);
        }

        let mut data = Vec::new();
        for range in ranges {
            let mut values = vec![0; (range.end - range.start) as usize];
            core.read(range.start, &mut values)?;
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

    /// Store the dumped core to a file.
    pub fn store(&self, path: &Path) -> Result<(), CoreDumpError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
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
