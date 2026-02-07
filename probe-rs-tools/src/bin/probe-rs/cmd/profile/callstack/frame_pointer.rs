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
