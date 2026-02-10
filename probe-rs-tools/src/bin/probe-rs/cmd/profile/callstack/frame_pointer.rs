use probe_rs::InstructionSet;
use probe_rs::MemoryInterface;
use probe_rs::RegisterValue;
use probe_rs_debug::unwind_program_counter_register;

use super::FunctionAddress;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FramePointerStackWalkError {
    #[error("Could not determine instruction set")]
    DetermineInstructionSet(#[source] probe_rs::Error),
    #[error("Could not read register")]
    ReadAddressOverflow,
    #[error("Could not read register")]
    ReadRegister(#[source] probe_rs::Error),
    #[error("Could not read frame record memory")]
    ReadMemory(#[source] probe_rs::Error),
    #[error("Failed to spill registers")]
    RegisterSpillError(#[source] probe_rs::Error),
}

/// Frame record contents for 32-bit cores
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRecord32 {
    /// Content of frame or stack pointer part of frame record
    frame_pointer: u32,
    return_address: u32,
}

/// Frame record contents for 64-bit cores
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FrameRecord64 {
    /// Content of frame or stack pointer part of frame record
    frame_pointer: u64,
    return_address: u64,
}

/// Attempt to read ARM A32 or RISCV32 frame record
fn read_arm_riscv_32_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u64,
    frame_record_offset: i64,
) -> Result<FrameRecord32, FramePointerStackWalkError> {
    let read_addr = frame_pointer
        .checked_add_signed(frame_record_offset)
        .ok_or(FramePointerStackWalkError::ReadAddressOverflow)?;
    let mut frame_record = [0; 2];
    memory
        .read_32(read_addr, &mut frame_record)
        .map_err(FramePointerStackWalkError::ReadMemory)?;

    let [caller_fp, return_address] = frame_record;

    Ok(FrameRecord32 {
        frame_pointer: caller_fp,
        return_address,
    })
}

/// Attempt to read ARM A64 or RISCV64 frame record
fn read_arm_riscv_64_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u64,
    frame_record_offset: i64,
) -> Result<FrameRecord64, FramePointerStackWalkError> {
    let read_addr = frame_pointer
        .checked_add_signed(frame_record_offset)
        .ok_or(FramePointerStackWalkError::ReadAddressOverflow)?;
    let mut frame_record = [0; 2];
    memory
        .read_64(read_addr, &mut frame_record)
        .map_err(FramePointerStackWalkError::ReadMemory)?;

    let [caller_fp, return_address] = frame_record;

    Ok(FrameRecord64 {
        frame_pointer: caller_fp,
        return_address,
    })
}

/// Attempt to read Xtensa frame record
fn read_xtensa_frame_record(
    memory: &mut dyn MemoryInterface,
    frame_pointer: u64,
    frame_record_offset: i64,
) -> Result<FrameRecord32, FramePointerStackWalkError> {
    let read_addr = frame_pointer
        .checked_add_signed(frame_record_offset)
        .ok_or(FramePointerStackWalkError::ReadAddressOverflow)?;

    let mut frame_record = [0; 2];
    memory
        .read_32(read_addr, &mut frame_record)
        .map_err(FramePointerStackWalkError::ReadMemory)?;

    // ra and fp are the other way round for Xtensa
    let [return_address, caller_fp] = frame_record;

    Ok(FrameRecord32 {
        frame_pointer: caller_fp,
        return_address,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AdjustedFrameRecord {
    frame_pointer: u64,
    adjusted_return_address: u64,
}

impl AdjustedFrameRecord {
    fn new_from_frame_record_32(
        fr_32: FrameRecord32,
        instruction_set: InstructionSet,
        last_pc: u64,
    ) -> Self {
        let ra = RegisterValue::U32(fr_32.return_address);

        let adjusted_return_address = if ra.is_zero() || ra.is_max_value() {
            ra
        } else {
            unwind_program_counter_register(ra, last_pc, Some(instruction_set))
                .expect("Valid return address unwound")
        };

        Self {
            frame_pointer: fr_32.frame_pointer as u64,
            adjusted_return_address: adjusted_return_address
                .try_into()
                .expect("Should be able to convert 32-bit return address to u64"),
        }
    }

    fn new_from_frame_record_64(
        fr_64: FrameRecord64,
        instruction_set: InstructionSet,
        last_pc: u64,
    ) -> Self {
        let ra = RegisterValue::U64(fr_64.return_address);

        let adjusted_return_address = if ra.is_zero() || ra.is_max_value() {
            ra
        } else {
            unwind_program_counter_register(ra, last_pc, Some(instruction_set))
                .expect("Valid return address unwound")
        };

        Self {
            frame_pointer: fr_64.frame_pointer,
            adjusted_return_address: adjusted_return_address
                .try_into()
                .expect("Should be able to convert 64-bit return address to u64"),
        }
    }
}

const ARM32_FRAME_RECORD_OFFSET: i64 = 0;
const ARM64_FRAME_RECORD_OFFSET: i64 = 0;
const RISCV32_FRAME_RECORD_OFFSET: i64 = -8;
const XTENSA_FRAME_RECORD_OFFSET: i64 = -16;

fn read_frame_record_for_core(
    memory: &mut dyn MemoryInterface,
    instruction_set: InstructionSet,
    frame_pointer: u64,
    last_pc: u64,
) -> Result<AdjustedFrameRecord, FramePointerStackWalkError> {
    match instruction_set {
        InstructionSet::A32 | InstructionSet::Thumb2 => {
            read_arm_riscv_32_frame_record(memory, frame_pointer, ARM32_FRAME_RECORD_OFFSET).map(
                |fr| AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc),
            )
        }
        InstructionSet::A64 => {
            read_arm_riscv_64_frame_record(memory, frame_pointer, ARM64_FRAME_RECORD_OFFSET).map(
                |fr| AdjustedFrameRecord::new_from_frame_record_64(fr, instruction_set, last_pc),
            )
        }
        InstructionSet::RV32 | InstructionSet::RV32C => {
            read_arm_riscv_32_frame_record(memory, frame_pointer, RISCV32_FRAME_RECORD_OFFSET).map(
                |fr| AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc),
            )
        }
        InstructionSet::Xtensa => {
            read_xtensa_frame_record(memory, frame_pointer, XTENSA_FRAME_RECORD_OFFSET).map(|fr| {
                AdjustedFrameRecord::new_from_frame_record_32(fr, instruction_set, last_pc)
            })
        }
    }
}

/// Part of frame pointer stack walk that is generic for memory interface, used for
/// frame_pointer_stack_walk implementationa and testing.
fn frame_pointer_stack_walk_memory_interface(
    memory: &mut dyn MemoryInterface,
    instruction_set: InstructionSet,
    entry_point_address_range: &std::ops::Range<u64>,
    program_counter: u64,
    mut frame_pointer: u64,
) -> Result<Vec<FunctionAddress>, FramePointerStackWalkError> {
    let mut stack_frames = Vec::new();
    stack_frames.push(FunctionAddress::ProgramCounter(program_counter));

    let mut last_frame_pointer = frame_pointer;
    let mut last_program_counter = program_counter;

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
        } = read_frame_record_for_core(
            memory,
            instruction_set,
            frame_pointer,
            last_program_counter,
        )?;

        // Stack grows down, so frame pointer should be increasing when walking up call stack
        // Stop if the frame pointer has not increased
        if last_frame_pointer >= frame_pointer {
            break;
        }
        last_frame_pointer = frame_pointer;

        stack_frames.push(FunctionAddress::AdjustedReturnAddress(
            adjusted_return_address,
        ));

        // Stop if the return address was in the entry point function
        if entry_point_address_range.contains(&adjusted_return_address) {
            break;
        }

        last_program_counter = adjusted_return_address;
    }

    Ok(stack_frames.into_iter().rev().collect())
}

pub(crate) fn frame_pointer_stack_walk<'a>(
    core: &mut probe_rs::Core<'a>,
    entry_point_address_range: &std::ops::Range<u64>,
) -> Result<Vec<FunctionAddress>, FramePointerStackWalkError> {
    let instruction_set = core
        .instruction_set()
        .map_err(FramePointerStackWalkError::DetermineInstructionSet)?;

    // Spill registers on Xtensa to ensure frame records are on the stack
    if instruction_set == InstructionSet::Xtensa {
        core.spill_registers()
            .map_err(FramePointerStackWalkError::RegisterSpillError)?;
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
        .map_err(FramePointerStackWalkError::ReadRegister)?;
    let program_counter: u64 = core
        .read_core_reg(core.program_counter())
        .map_err(FramePointerStackWalkError::ReadRegister)?;

    frame_pointer_stack_walk_memory_interface(
        core,
        instruction_set,
        entry_point_address_range,
        program_counter,
        frame_pointer,
    )
}

#[cfg(test)]
mod test {
    use probe_rs_debug::DebugRegisters;

    use probe_rs::{CoreDump, RegisterRole};
    use std::path::PathBuf;

    use super::super::test::{addresses_to_callstack, get_path_for_test_files};
    use super::*;

    /// Find core dump file path using name - either .elf or probe-rs's .coredump format
    fn coredump_path(base: &str) -> PathBuf {
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

    /// Like `frame_pointer_stack_walk` but for CoreDump rather than Core
    fn frame_pointer_stack_walk_core_dump(
        core_dump: &mut CoreDump,
        entry_point_address_range: &std::ops::Range<u64>,
    ) -> Result<Vec<FunctionAddress>, FramePointerStackWalkError> {
        // I hope Xtensa registers are already spilled in core dump

        let instruction_set = core_dump.instruction_set();

        // Use the stack pointer on Xtensa as compiling with force-frame-pointers=yes can be buggy
        // and we spill registers to the stack so we guarantee the frame record starts at SP - 16.
        // Use frame pointer on all other architectures.
        let fp_role = match instruction_set {
            InstructionSet::Xtensa => RegisterRole::StackPointer,
            _ => RegisterRole::FramePointer,
        };

        let initial_registers = DebugRegisters::from_coredump(core_dump);
        let frame_pointer = initial_registers
            .get_register_value_by_role(&fp_role)
            .unwrap();
        let program_counter = initial_registers
            .get_register_value_by_role(&RegisterRole::ProgramCounter)
            .unwrap();

        frame_pointer_stack_walk_memory_interface(
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
        let coredump_path = coredump_path(&format!("debug-unwind-tests/{test_name}"));

        let mut core_dump = CoreDump::load(&coredump_path).unwrap();
        let object_bytes = std::fs::read(&executable_location).unwrap();
        let obj = object::File::parse(object_bytes.as_slice()).unwrap();
        let entry_point_address_range = super::super::get_entry_point_address_range(&obj).unwrap();

        let res =
            frame_pointer_stack_walk_core_dump(&mut core_dump, &entry_point_address_range).unwrap();

        assert_eq!(&res, expect);
    }

    /// frame_pointer_stack_walk RISC-V coredump in ELF format from esp32c6
    #[test]
    fn test_frame_pointer_stack_walk_riscv32() {
        // the frame pointer register happens to point to the correct place in this core dump
        let test_name = "esp32c6_coredump_elf";
        let expect = addresses_to_callstack(&[
            0x4200124e, // rust_begin_unwind
            0x420054f2, // _ZN4core9panicking9panic_fmt17h021b089f2ed24437E
            0x42000202, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17hcf2d0b9f6da05190E
            0x420052ec, // _ZN16embassy_executor3raw8Executor4poll17h95bc77c9558ed726E
            0x42000244, // _ZN15esp_hal_embassy8executor6thread8Executor3run17h70decec90d969805E
            0x42000510, // main
            0x4200438c, // hal_main
            0x42000132, // _start_rust
        ]);
        check_stack_walk(test_name, &expect);
    }

    /// frame_pointer_stack_walk Armv7-em coredump from atsamd51p19a
    #[test]
    fn test_frame_pointer_stack_walk_armv7em() {
        let test_name = "atsamd51p19a";
        let expect = addresses_to_callstack(&[
            0x1474, // print_const_pointers
            0x14da, // print_pointers
            0x1538, // main
            0x978,  // Reset_Handler
        ]);
        check_stack_walk(test_name, &expect);
    }

    /// frame_pointer_stack_walk Xtensa coredump from esp32s3
    #[test]
    fn test_frame_pointer_stack_walk_xtensa() {
        let test_name = "esp32s3_coredump_elf";
        let expect = addresses_to_callstack(&[
            0x420045e3, // rust_begin_unwind
            // frame missed - 0x4200587a
            0x42000f69, // _ZN11coredump_c67do_loop17hf978f6cd1e9a91bbE
            0x42000d10, // _ZN16embassy_executor3raw20TaskStorage$LT$F$GT$4poll17h82f24e86eebf8c70E.llvm.2709420154441022049
            0x42004c4e, // _ZN16embassy_executor3raw8Executor4poll17h6968ad0e84efef64E
            0x42000bb9, // _ZN15esp_hal_embassy8executor6thread8Executor3run17h3be5e460a364c27eE
            0x42000f7f, // main
            0x42004483, // Reset
            0x40378836, // ESP32Reset
        ]);
        check_stack_walk(test_name, &expect);
    }
}
