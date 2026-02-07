use probe_rs::InstructionSet;
use probe_rs::MemoryInterface;
use probe_rs::RegisterValue;
use probe_rs_debug::DebugError;
use probe_rs_debug::frame_record;
use probe_rs_debug::unwind_program_counter_register;

use super::FunctionAddress;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FramePointerUnwindError {
    #[error("Could not determine instruction set")]
    DetermineInstructionSet(#[source] probe_rs::Error),
    #[error("Could not read register")]
    ReadRegister(#[source] probe_rs::Error),
    #[error("Failed to spill registers")]
    RegisterSpillError(#[source] probe_rs::Error),
    #[error("Could not read frame record")]
    ReadFrameRecord(#[source] probe_rs_debug::DebugError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdjustedFrameRecord {
    frame_pointer: u64,
    adjusted_return_address: u64,
}

impl AdjustedFrameRecord {
    fn new_from_frame_record_32(
        fr_32: frame_record::FrameRecord32,
        instruction_set: InstructionSet,
        last_pc: u64,
    ) -> Self {
        let ra = RegisterValue::U32(fr_32.return_address);

        let adjusted_return_address = if ra.is_zero() || ra.is_max_value() {
            ra
        } else {
            let (adjusted_ra_opt, _) =
                unwind_program_counter_register(ra, last_pc, Some(instruction_set))
                    .expect("Valid return address unwound");
            adjusted_ra_opt
        };

        Self {
            frame_pointer: fr_32.frame_pointer as u64,
            adjusted_return_address: adjusted_return_address
                .try_into()
                .expect("Should be able to convert 32-bit return address to u64"),
        }
    }

    fn new_from_frame_record_64(
        fr_64: frame_record::FrameRecord64,
        instruction_set: InstructionSet,
        last_pc: u64,
    ) -> Self {
        let ra = RegisterValue::U64(fr_64.return_address);

        let adjusted_return_address = if ra.is_zero() || ra.is_max_value() {
            ra
        } else {
            let (adjusted_ra_opt, _) =
                unwind_program_counter_register(ra, last_pc, Some(instruction_set))
                    .expect("Valid return address unwound");
            adjusted_ra_opt
        };

        Self {
            frame_pointer: fr_64.frame_pointer,
            adjusted_return_address: adjusted_return_address
                .try_into()
                .expect("Should be able to convert 64-bit return address to u64"),
        }
    }
}

fn read_frame_record_for_core(
    memory: &mut dyn MemoryInterface,
    instruction_set: InstructionSet,
    frame_pointer: u64,
    last_pc: u64,
) -> Result<AdjustedFrameRecord, DebugError> {
    match instruction_set {
        InstructionSet::A32 | InstructionSet::Thumb2 => {
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_arm32_frame_record(memory, fp_32).map(|fr| {
                AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc)
            })
        }
        InstructionSet::A64 => frame_record::read_arm64_frame_record(memory, frame_pointer)
            .map(|fr| AdjustedFrameRecord::new_from_frame_record_64(fr, instruction_set, last_pc)),
        InstructionSet::RV32 | InstructionSet::RV32C => {
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_riscv32_frame_record(memory, fp_32).map(|fr| {
                AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc)
            })
        }
        InstructionSet::Xtensa => {
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_xtensa_frame_record(memory, fp_32).map(|fr| {
                AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc)
            })
        }
    }
}

/// Part of frame pointer unwind that is generic for memory interface, used for
/// frame_pointer_unwind implementationa and testing.
fn frame_pointer_unwind_memory_interface(
    memory: &mut dyn MemoryInterface,
    instruction_set: InstructionSet,
    entry_point_address_range: &std::ops::Range<u64>,
    mut program_counter: u64,
    mut frame_pointer: u64,
) -> Result<Vec<FunctionAddress>, FramePointerUnwindError> {
    let mut stack_frames = Vec::new();

    stack_frames.push(FunctionAddress::ProgramCounter(program_counter));

    // Unwind, stopping if:
    //
    // - Return address is in the entry point address range
    // - Frame pointer is 0:
    //   - For arm32/aarch64 section 6.2.1.4 of the AAPCS32 / 6.4.6 of the AAPCS64 states:
    //   "The end of the frame record chain is indicated by the address zero in the address for the
    //   previous frame." - https://github.com/ARM-software/abi-aa/releases
    //   Most startup code does not implement this though.
    //   - Version 1.1 (pre-release), section 1.2 of the RISC-V ABI states:
    //   "The end of the frame record chain is indicated by the address zero appearing as the next
    //   link in the chain." - https://github.com/riscv-non-isa/riscv-elf-psabi-doc/releases
    while frame_pointer != 0 {
        let adjusted_return_address;
        AdjustedFrameRecord {
            frame_pointer,
            adjusted_return_address,
        } = read_frame_record_for_core(memory, instruction_set, frame_pointer, program_counter)
            .map_err(FramePointerUnwindError::ReadFrameRecord)?;

        stack_frames.push(FunctionAddress::AdjustedReturnAddress(
            adjusted_return_address,
        ));

        // Stop if the return address was in the entry point function
        if entry_point_address_range.contains(&adjusted_return_address) {
            break;
        }

        program_counter = adjusted_return_address;
    }

    Ok(stack_frames.into_iter().rev().collect())
}

pub(crate) fn frame_pointer_unwind<'a>(
    core: &mut probe_rs::Core<'a>,
    entry_point_address_range: &std::ops::Range<u64>,
) -> Result<Vec<FunctionAddress>, FramePointerUnwindError> {
    let instruction_set = core
        .instruction_set()
        .map_err(FramePointerUnwindError::DetermineInstructionSet)?;

    // Spill registers on Xtensa to ensure frame records are on the stack
    if instruction_set == InstructionSet::Xtensa {
        core.spill_registers()
            .map_err(FramePointerUnwindError::RegisterSpillError)?;
    }

    // Use the stack pointer on Xtensa as compiling with force-frame-pointers=yes can be buggy
    // and we spill registers to the stack so we guarantee the frame record starts at SP - 16.
    // Use frame pointer on all other architectures.
    let fp_reg = match instruction_set {
        InstructionSet::Xtensa => core.stack_pointer(),
        _ => core.frame_pointer(),
    };

    let frame_pointer: u64 = core
        .read_core_reg(fp_reg)
        .map_err(FramePointerUnwindError::ReadRegister)?;
    let program_counter: u64 = core
        .read_core_reg(core.program_counter())
        .map_err(FramePointerUnwindError::ReadRegister)?;

    frame_pointer_unwind_memory_interface(
        core,
        instruction_set,
        entry_point_address_range,
        program_counter,
        frame_pointer,
    )
}

#[cfg(test)]
mod test {
    use probe_rs_debug::{DebugInfo, DebugRegisters};

    use probe_rs::{CoreDump, RegisterRole};
    use std::path::PathBuf;

    use super::*;

    /// Get the full path to a file in the `tests` directory.
    fn get_path_for_test_files(relative_file: &str) -> PathBuf {
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.pop();
        path.push("probe-rs-debug");
        path.push("tests");
        path.push(relative_file);
        path
    }

    /// Load the DebugInfo from the `elf_file` for the test.
    /// `elf_file` should be the name of a file(or relative path) in the `tests` directory.
    fn load_test_elf_as_debug_info(elf_file: &str) -> DebugInfo {
        let path = get_path_for_test_files(elf_file);
        DebugInfo::from_file(&path).unwrap_or_else(|err: DebugError| {
            panic!("Failed to open file {}: {:?}", path.display(), err)
        })
    }

    fn coredump_path(base: String) -> PathBuf {
        let possible_coredump_paths = [
            get_path_for_test_files(format!("{base}.coredump").as_str()),
            get_path_for_test_files(format!("{base}_coredump.elf").as_str()),
        ];

        possible_coredump_paths
            .iter()
            .find(|path| path.exists())
            .unwrap_or_else(|| {
                panic!(
                    "No coredump found for chip {base}. Expected one of: {possible_coredump_paths:?}"
                )
            })
            .clone()
    }

    /// Like `frame_pointer_unwind` but for CoreDump rather than Core
    pub(crate) fn frame_pointer_unwind_core_dump(
        core_dump: &mut CoreDump,
        entry_point_address_range: &std::ops::Range<u64>,
    ) -> Result<Vec<FunctionAddress>, FramePointerUnwindError> {
        // I hope Xtensa registers are already spilled in core dump

        let instruction_set = core_dump.instruction_set();

        // Use the stack pointer on Xtensa as compiling with force-frame-pointers=yes can be buggy
        // and we spill registers to the stack so we guarantee the frame record starts at SP - 16.
        // Use frame pointer on all other architectures.
        let fp_role = match instruction_set {
            InstructionSet::Xtensa => RegisterRole::StackPointer,
            _ => RegisterRole::FramePointer,
        };

        let initial_registers = DebugRegisters::from_coredump(&core_dump);
        let frame_pointer = initial_registers
            .get_register_value_by_role(&fp_role)
            .unwrap();
        let program_counter = initial_registers
            .get_register_value_by_role(&RegisterRole::ProgramCounter)
            .unwrap();

        frame_pointer_unwind_memory_interface(
            core_dump,
            instruction_set,
            entry_point_address_range,
            program_counter,
            frame_pointer,
        )
    }

    fn check_stack_walk(test_name: &str, expect: &Vec<FunctionAddress>) {
        let executable_location =
            get_path_for_test_files(format!("debug-unwind-tests/{test_name}.elf").as_str());
        let coredump_path = coredump_path(format!("debug-unwind-tests/{test_name}"));

        let mut core_dump = CoreDump::load(&coredump_path).unwrap();
        let object_bytes = std::fs::read(&executable_location).unwrap();
        let obj = object::File::parse(object_bytes.as_slice()).unwrap();
        let entry_point_address_range = super::super::get_entry_point_address_range(&obj).unwrap();

        let res =
            frame_pointer_unwind_core_dump(&mut core_dump, &entry_point_address_range).unwrap();

        assert_eq!(&res, expect);
    }

    /// Helper to convert slice of addresses to callstack Vec
    fn addresses_to_callstack(addresses: &[u64]) -> Vec<FunctionAddress> {
        addresses
            .iter()
            .copied()
            .enumerate()
            .map(|(i, val)| match i {
                0 => FunctionAddress::ProgramCounter(val),
                _ => FunctionAddress::AdjustedReturnAddress(val),
            })
            .rev()
            .collect()
    }

    /// frame_pointer_unwind Armv6-m using RP2040
    #[test]
    fn test_frame_pointer_unwind_armv6m() {
        let test_name = "RP2040_full_unwind";
        let expect = Vec::new();
        check_stack_walk(&test_name, &expect);
    }

    /// frame_pointer_unwind Xtensa using esp32s3
    #[test]
    fn test_frame_pointer_unwind_xtensa() {
        let test_name = "esp32s3_coredump_elf";
        let expect = addresses_to_callstack(&[
            0x420045e3, // frame missed - 0x4200587a
            0x42000f69, 0x42000d10, 0x42004c4e, 0x42000bb9, 0x42000f7f, 0x42004483, 0x40378836,
        ]);
        check_stack_walk(&test_name, &expect);
    }

    // #[test_case("nRF52833_xxAA_full_unwind"; "full_unwind Armv7-m using nRF52833_xxAA")]
    // #[test_case("atsamd51p19a"; "Armv7-em from C source code")]
    // #[test_case("esp32c3_full_unwind"; "full_unwind RISC-V32E using esp32c3")]
    // #[test_case("esp32c6_coredump_elf"; "Unwind using a RISC-V coredump in ELF format")]
}
