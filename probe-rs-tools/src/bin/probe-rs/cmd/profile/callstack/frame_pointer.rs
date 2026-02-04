use probe_rs::InstructionSet;
use probe_rs_debug::DebugError;
use probe_rs_debug::frame_record;

use super::StackFrameInfo;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FramePointerUnwindError {
    #[error("Could not determine instruction set")]
    DetermineInstructionSet(#[source] probe_rs::Error),
    #[error("Could not read register")]
    ReadRegister(#[source] probe_rs::Error),
    #[error("Could not read frame record")]
    ReadFrameRecord(#[source] probe_rs_debug::DebugError),
}

fn frame_record_32_to_64(fr_32: frame_record::FrameRecord32) -> frame_record::FrameRecord64 {
    frame_record::FrameRecord64 {
        frame_pointer: fr_32.frame_pointer.into(),
        return_address: fr_32.return_address.into(),
    }
}

fn read_frame_record_for_core<'a>(
    core: &mut probe_rs::Core<'a>,
    instruction_set: InstructionSet,
    frame_pointer: u64,
) -> Result<frame_record::FrameRecord64, DebugError> {
    match instruction_set {
        InstructionSet::A32 | InstructionSet::Thumb2 => {
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_arm32_frame_record(core, fp_32).map(frame_record_32_to_64)
        }
        InstructionSet::A64 => frame_record::read_arm64_frame_record(core, frame_pointer),
        InstructionSet::RV32 | InstructionSet::RV32C => {
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_riscv32_frame_record(core, fp_32).map(frame_record_32_to_64)
        }
        InstructionSet::Xtensa => {
            core.spill_registers()?;
            let fp_32 = frame_pointer
                .try_into()
                .map_err(|_| DebugError::Other("Expected 32 bit frame pointer".to_string()))?;
            frame_record::read_xtensa_frame_record(core, fp_32).map(frame_record_32_to_64)
        }
    }
}

pub(crate) fn frame_pointer_unwind<'a>(
    core: &mut probe_rs::Core<'a>,
    entry_point_address_range: &std::ops::Range<u64>,
) -> Result<Vec<StackFrameInfo>, FramePointerUnwindError> {
    let mut stack_frames = Vec::new();

    let instruction_set = core
        .instruction_set()
        .map_err(FramePointerUnwindError::DetermineInstructionSet)?;

    // Use the stack pointer on Xtensa as compiling with force-frame-pointers=yes can be buggy
    // and we spill registers to the stack so we guarantee the frame record starts at SP - 16.
    // Use frame pointer on all other architectures.
    let fp_reg = match instruction_set {
        InstructionSet::Xtensa => core.stack_pointer(),
        _ => core.frame_pointer(),
    };

    let mut frame_pointer: u64 = core
        .read_core_reg(fp_reg)
        .map_err(FramePointerUnwindError::ReadRegister)?;
    let program_counter: u64 = core
        .read_core_reg(core.program_counter())
        .map_err(FramePointerUnwindError::ReadRegister)?;

    stack_frames.push(StackFrameInfo::ProgramCounter(program_counter));

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
        let return_address;
        frame_record::FrameRecord64 {
            frame_pointer,
            return_address,
        } = read_frame_record_for_core(core, instruction_set, frame_pointer)
            .map_err(FramePointerUnwindError::ReadFrameRecord)?;

        stack_frames.push(StackFrameInfo::ReturnAddress(return_address));

        // Stop if the return address was in the entry point function
        if entry_point_address_range.contains(&return_address) {
            break;
        }
    }

    Ok(stack_frames.into_iter().rev().collect())
}
