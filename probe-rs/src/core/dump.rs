use crate::{Core, CoreType, Error, InstructionSet, MemoryInterface};
use crate::{RegisterId, RegisterValue};
use probe_rs_target::MemoryRange;
use scroll::Cread;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    ops::Range,
    path::{Path, PathBuf},
};

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
    ) -> Result<&[u8], crate::Error> {
        for (range, memory) in &self.data {
            if range.contains_range(&(address..(address + size_in_bytes))) {
                let offset = (address - range.start) as usize;

                return Ok(&memory[offset..][..size_in_bytes as usize]);
            }
        }
        // If we get here, then no range with the requested memory address and size was found.
        Err(crate::Error::Other(format!(
            "The coredump does not include the memory for address {address:#x} of size {size_in_bytes:#x}"
        )))
    }

    /// Read the requested memory range from the coredump, and return the data in the requested buffer.
    /// The word-size of the read is determined by the size of the items in the `data` buffer.
    fn read_memory_range<T>(&self, address: u64, data: &mut [T]) -> Result<(), crate::Error>
    where
        T: scroll::ctx::FromCtx<scroll::Endian>,
    {
        let memory =
            self.get_memory_from_coredump(address, (std::mem::size_of_val(data)) as u64)?;

        let value_size = std::mem::size_of::<T>();

        for (n, data) in data.iter_mut().enumerate() {
            *data = memory.cread_with::<T>(n * value_size, scroll::LE);
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
