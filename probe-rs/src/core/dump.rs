use crate::architecture::arm::core::registers::aarch32::{
    AARCH32_CORE_REGISTERS, AARCH32_WITH_FP_16_CORE_REGISTERS, AARCH32_WITH_FP_32_CORE_REGISTERS,
};
use crate::architecture::arm::core::registers::aarch64::AARCH64_CORE_REGISTERS;
use crate::architecture::arm::core::registers::cortex_m::{
    CORTEX_M_CORE_REGISTERS, CORTEX_M_WITH_FP_CORE_REGISTERS,
};
use crate::architecture::riscv::registers::{RISCV_CORE_REGISTERS, RISCV_WITH_FP_CORE_REGISTERS};
use crate::architecture::xtensa::arch::{Register as XtensaRegister, SpecialRegister};
use crate::architecture::xtensa::registers::XTENSA_CORE_REGISTERS;
use crate::{Core, CoreRegisters, CoreType, Error, InstructionSet, MemoryInterface};
use crate::{RegisterId, RegisterValue};
use object::elf::PT_NOTE;
use object::read::elf::ProgramHeader;
use object::{Object, ObjectSegment};
use probe_rs_target::MemoryRange;
use scroll::Cread;
use serde::{Deserialize, Serialize};
use std::array;
use std::sync::LazyLock;
use std::{
    collections::HashMap,
    fs::OpenOptions,
    ops::Range,
    path::{Path, PathBuf},
};

trait Processor {
    /// Returns the instruction set of the processor.
    fn instruction_set(&self) -> InstructionSet;

    /// Returns the core type of the processor.
    fn core_type(&self) -> CoreType;

    /// Returns whether the processor supports native 64 bit access.
    fn supports_native_64bit_access(&self) -> bool {
        false
    }

    /// Returns the length of the register data in bytes.
    fn register_data_len(&self) -> usize;

    /// Returns the core registers and their positions in the register data.
    ///
    /// The positions are in register indexes, not byte offsets.
    fn register_map(&self) -> &[(usize, RegisterId)];

    /// Reads the registers from the .elf note data and stores them in the provided map.
    fn read_registers(
        &self,
        note_data: &[u8],
        registers: &mut HashMap<RegisterId, RegisterValue>,
    ) -> Result<(), CoreDumpError> {
        for (offset, reg_id) in self.register_map().iter().copied() {
            let value = self.read_register(note_data, offset)?;
            registers.insert(reg_id, value);
        }

        Ok(())
    }

    /// Reads a single register value from the note data. The register is addressed by its
    /// position in the .elf note data.
    fn read_register(&self, note_data: &[u8], idx: usize) -> Result<RegisterValue, CoreDumpError> {
        let value = u32::from_le_bytes(note_data[idx * 4..][..4].try_into().unwrap());
        Ok(RegisterValue::U32(value))
    }
}

struct XtensaProcessor;
impl Processor for XtensaProcessor {
    fn instruction_set(&self) -> InstructionSet {
        InstructionSet::Xtensa
    }
    fn core_type(&self) -> CoreType {
        CoreType::Xtensa
    }
    fn register_data_len(&self) -> usize {
        128 * 4
    }
    fn register_map(&self) -> &[(usize, RegisterId)] {
        static REGS: LazyLock<[(usize, RegisterId); 24]> = LazyLock::new(|| {
            let core_regs = &XTENSA_CORE_REGISTERS;

            array::from_fn(|idx| {
                match idx {
                    // First 8 registers are special registers.
                    0 => (idx, RegisterId::from(XtensaRegister::CurrentPc)),
                    1 => (idx, RegisterId::from(SpecialRegister::Ps)),
                    2 => (idx, RegisterId::from(SpecialRegister::Lbeg)),
                    3 => (idx, RegisterId::from(SpecialRegister::Lend)),
                    4 => (idx, RegisterId::from(SpecialRegister::Lcount)),
                    5 => (idx, RegisterId::from(SpecialRegister::Sar)),
                    6 => (idx, RegisterId::from(SpecialRegister::Windowstart)),
                    7 => (idx, RegisterId::from(SpecialRegister::Windowbase)),
                    // There are 56 reserved registers before the core registers.
                    8..24 => {
                        // The coredump contains all 64 AR registers but we don't define them yet,
                        // so let's just use the 16 of the visible window.
                        let ar_idx = idx - 8;
                        (ar_idx + 64, core_regs.core_register(ar_idx).id())
                    }
                    _ => unreachable!(),
                }
            })
        });

        &*REGS
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
    fn register_data_len(&self) -> usize {
        32 * 4
    }
    fn register_map(&self) -> &[(usize, RegisterId)] {
        static REGS: LazyLock<[(usize, RegisterId); 32]> = LazyLock::new(|| {
            let core_regs = &RISCV_CORE_REGISTERS;

            array::from_fn(|idx| {
                // Core register 0 is the "zero" register. Coredumps place the PC there instead.
                let regid = if idx == 0 {
                    core_regs.pc().unwrap().id()
                } else {
                    core_regs.core_register(idx).id()
                };
                (idx, regid)
            })
        });
        &*REGS
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
            instruction_set: core.instruction_set()?,
            supports_native_64bit_access: core.supports_native_64bit_access(),
            core_type: core.core_type(),
            fpu_support: core.fpu_support()?,
            floating_point_register_count: Some(core.floating_point_register_count()?),
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
        // `elf.segments()` returns PT_LOAD segments only.
        for segment in elf.segments() {
            let address: u64 = segment.elf_program_header().p_vaddr(endianness).into();
            let size: u64 = segment.elf_program_header().p_memsz(endianness).into();
            let memory = segment.data()?;
            tracing::debug!(
                "Adding memory segment: {:#x} - {:#x}",
                address,
                address + size
            );
            data.push((address..address + size, memory.to_vec()));
        }

        // Registers are in a Note segment.
        let Some(register_note) = elf
            .elf_program_headers()
            .iter()
            .find(|s| s.p_type(endianness) == PT_NOTE)
        else {
            return Err(CoreDumpError::DecodingElfCoreDump(
                "No note segment found".to_string(),
            ));
        };

        let mut registers = HashMap::new();
        for note in register_note
            .notes(endianness, elf_data)?
            .expect("Failed to read notes from a PT_NOTE segment. This is a bug, please report it.")
        {
            let note = note?;
            if note.name() != b"CORE" {
                continue;
            }

            // The CORE note contains some thread-specific information before/after the registers.
            // We only care about the registers, so let's cut off the rest. If we decide to use
            // the other information, we can do that later, most likely without
            // architecture-specific processing code.
            const CORE_NOTE_HEADER_SIZE: usize = 72;
            let note_length = processor.register_data_len();

            if note.desc().len() < CORE_NOTE_HEADER_SIZE + note_length {
                return Err(CoreDumpError::DecodingElfCoreDump(format!(
                    "Note segment is too small: {} bytes instead of at least {}",
                    note.desc().len(),
                    CORE_NOTE_HEADER_SIZE + note_length
                )));
            }

            let note_data = &note.desc()[CORE_NOTE_HEADER_SIZE..][..note_length];
            processor.read_registers(note_data, &mut registers)?;
        }

        Ok(Self {
            registers,
            data,
            instruction_set: processor.instruction_set(),
            supports_native_64bit_access: processor.supports_native_64bit_access(),
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

    /// Returns the register map for the core type.
    pub fn registers(&self) -> &'static CoreRegisters {
        match self.core_type {
            CoreType::Armv6m => &CORTEX_M_CORE_REGISTERS,
            CoreType::Armv7a => match self.floating_point_register_count {
                Some(16) => &AARCH32_WITH_FP_16_CORE_REGISTERS,
                Some(32) => &AARCH32_WITH_FP_32_CORE_REGISTERS,
                _ => &AARCH32_CORE_REGISTERS,
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
            CoreType::Armv8a => &AARCH64_CORE_REGISTERS,
            CoreType::Armv8m => {
                if self.fpu_support {
                    &CORTEX_M_WITH_FP_CORE_REGISTERS
                } else {
                    &CORTEX_M_CORE_REGISTERS
                }
            }
            CoreType::Riscv => {
                if self.fpu_support {
                    &RISCV_WITH_FP_CORE_REGISTERS
                } else {
                    &RISCV_CORE_REGISTERS
                }
            }
            CoreType::Xtensa => &XTENSA_CORE_REGISTERS,
        }
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
    /// Invalid ELF file.
    #[error("Invalid ELF file.")]
    ElfCoreDumpFormat(#[from] object::read::Error),
}
