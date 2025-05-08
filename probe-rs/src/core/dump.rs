use crate::architecture::riscv::registers::RISCV_CORE_REGISTERS;
use crate::architecture::xtensa::arch::{Register as XtensaRegister, SpecialRegister};
use crate::architecture::xtensa::registers::XTENSA_CORE_REGISTERS;
use crate::{Core, CoreType, Error, InstructionSet, MemoryInterface};
use crate::{RegisterId, RegisterValue};
use object::read::elf::ProgramHeader;
use object::{Object, ObjectSegment};
use probe_rs_target::MemoryRange;
use scroll::Cread;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::OpenOptions,
    ops::Range,
    path::{Path, PathBuf},
};

trait Processor {
    fn instruction_set(&self) -> InstructionSet;
    fn core_type(&self) -> CoreType;
    fn read_registers(
        &self,
        note_data: &[u8],
        registers: &mut HashMap<RegisterId, RegisterValue>,
    ) -> Result<(), CoreDumpError>;
}

struct XtensaProcessor;
impl Processor for XtensaProcessor {
    fn instruction_set(&self) -> InstructionSet {
        InstructionSet::Xtensa
    }
    fn core_type(&self) -> CoreType {
        CoreType::Xtensa
    }
    fn read_registers(
        &self,
        note_data: &[u8],
        registers: &mut HashMap<RegisterId, RegisterValue>,
    ) -> Result<(), CoreDumpError> {
        let core_regs = &XTENSA_CORE_REGISTERS;
        let reg_idxs = [
            XtensaRegister::CurrentPc,
            XtensaRegister::Special(SpecialRegister::Ps),
            XtensaRegister::Special(SpecialRegister::Lbeg),
            XtensaRegister::Special(SpecialRegister::Lend),
            XtensaRegister::Special(SpecialRegister::Lcount),
            XtensaRegister::Special(SpecialRegister::Sar),
            XtensaRegister::Special(SpecialRegister::Windowstart),
            XtensaRegister::Special(SpecialRegister::Windowbase),
        ];

        let reg_by_idx = |idx| {
            RegisterValue::U32(u32::from_le_bytes(
                note_data[idx * 4..][..4].try_into().unwrap(),
            ))
        };

        for (idx, reg) in reg_idxs.into_iter().enumerate() {
            registers.insert(RegisterId::from(reg), reg_by_idx(idx));
        }
        for core_reg in 0..16 {
            registers.insert(
                RegisterId::from(core_regs.core_register(core_reg)),
                reg_by_idx(64 + core_reg),
            );
        }

        Ok(())
    }
}

struct RiscvProcessor;
impl Processor for RiscvProcessor {
    fn instruction_set(&self) -> InstructionSet {
        InstructionSet::RV32
    }
    fn core_type(&self) -> CoreType {
        CoreType::Riscv
    }
    fn read_registers(
        &self,
        note_data: &[u8],
        registers: &mut HashMap<RegisterId, RegisterValue>,
    ) -> Result<(), CoreDumpError> {
        let reg_by_idx = |idx| {
            RegisterValue::U32(u32::from_le_bytes(
                note_data[idx * 4..][..4].try_into().unwrap(),
            ))
        };

        let core_regs = &RISCV_CORE_REGISTERS;

        registers.insert(RegisterId::from(core_regs.pc().unwrap()), reg_by_idx(0));
        for core_reg in 1..32 {
            registers.insert(
                RegisterId::from(core_regs.core_register(core_reg)),
                reg_by_idx(core_reg),
            );
        }

        Ok(())
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
        let file_contents = std::fs::read(path).map_err(|e| {
            CoreDumpError::CoreDumpFileRead(e, dunce::canonicalize(path).unwrap_or_default())
        })?;
        Self::load_raw(&file_contents)
    }

    /// Load the dumped core from a file.
    pub fn load_raw(data: &[u8]) -> Result<Self, CoreDumpError> {
        if let Ok(elf) = object::read::elf::ElfFile32::parse(data) {
            Self::load_elf(elf)
        } else if let Ok(elf) = object::read::elf::ElfFile64::parse(data) {
            Self::load_elf(elf)
        } else {
            rmp_serde::from_slice(data).map_err(CoreDumpError::DecodingCoreDump)
        }
    }

    fn load_elf<Elf: object::read::elf::FileHeader<Endian = object::Endianness>>(
        elf: object::read::elf::ElfFile<'_, Elf>,
    ) -> Result<Self, CoreDumpError> {
        let endianness = elf.endianness();
        let elf_data = elf.data();

        let processor: Box<dyn Processor> = match elf.architecture() {
            object::Architecture::Riscv32 => Box::new(RiscvProcessor),
            object::Architecture::Xtensa => Box::new(XtensaProcessor),
            other => {
                return Err(CoreDumpError::DecodingElfCoreDump(format!(
                    "Unsupported architecture: {other:?}",
                )));
            }
        };

        // The memory is in a Load segment.
        let mut data = Vec::new();
        for segment in elf.segments() {
            let address: u64 = segment.elf_program_header().p_vaddr(endianness).into();
            let size: u64 = segment.elf_program_header().p_memsz(endianness).into();
            let memory = segment.data().unwrap();
            tracing::debug!(
                "Adding memory segment: {:#x} - {:#x}",
                address,
                address + size
            );
            data.push((address..address + size, memory.to_vec()));
        }

        // Registers are in a Note segment.
        let register_note = elf
            .elf_program_headers()
            .iter()
            .find(|s| s.p_type(endianness) == 4)
            .unwrap();

        let mut registers = HashMap::new();
        for note in register_note.notes(endianness, elf_data).unwrap().unwrap() {
            let note = note.unwrap();
            if note.name() != b"CORE" {
                continue;
            }

            let note_data = &note.desc()[72..];
            processor.read_registers(note_data, &mut registers)?;
        }

        Ok(Self {
            registers,
            data,
            instruction_set: processor.instruction_set(),
            supports_native_64bit_access: false,
            core_type: processor.core_type(),
            fpu_support: false,
            floating_point_register_count: None,
        })
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
    /// Decoding the coredump .elf failed.
    #[error("Decoding the coredump .elf failed.")]
    DecodingElfCoreDump(String),
}
