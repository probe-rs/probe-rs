use probe_rs::InstructionSet;
use probe_rs::MemoryInterface;

use super::StackFrameInfo;

#[derive(Debug, thiserror::Error)]
pub(crate) enum FramePointerUnwindError {
    #[error("Could not determine instruction set")]
    DetermineInstructionSet(#[source] probe_rs::Error),
    #[error("Could not read register")]
    ReadRegister(#[source] probe_rs::Error),
    #[error("Could not read memory address")]
    ReadMemory(#[source] probe_rs::Error),
    #[error("Unsupported instruction set: {0:?}")]
    UnsupportedInstructionSet(InstructionSet),
    #[error("Overflow while calculating next read address")]
    AddressOverflow,
}

fn read_mem<'a>(core: &mut probe_rs::Core<'a>, addr: u64) -> Result<u64, probe_rs::Error> {
    if core.is_64_bit() {
        core.read_word_64(addr)
    } else {
        core.read_word_32(addr).map(u64::from)
    }
}

/// Offsets of return address and next frame pointer from current frame pointer
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct FpUnwindOffsets {
    frame_pointer: i64,
    return_address: i64,
}

impl FpUnwindOffsets {
    fn new(instruction_set: &InstructionSet) -> Result<Self, FramePointerUnwindError> {
        match instruction_set {
            InstructionSet::A32 | InstructionSet::Thumb2 => Ok(Self {
                frame_pointer: 0,
                return_address: 4,
            }),
            InstructionSet::A64 => Ok(Self {
                frame_pointer: 0,
                return_address: 8,
            }),
            InstructionSet::RV32 | InstructionSet::RV32C => Ok(Self {
                frame_pointer: -8,
                return_address: -4,
            }),
            // not supporting xtensa yet because it's complicated
            _ => Err(FramePointerUnwindError::UnsupportedInstructionSet(
                *instruction_set,
            )),
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
    let offsets = FpUnwindOffsets::new(&instruction_set)?;

    let mut frame_pointer: u64 = core
        .read_core_reg(core.frame_pointer())
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
        let return_address_address = frame_pointer
            .checked_add_signed(offsets.return_address)
            .ok_or(FramePointerUnwindError::AddressOverflow)?;
        let return_address =
            read_mem(core, return_address_address).map_err(FramePointerUnwindError::ReadMemory)?;
        stack_frames.push(StackFrameInfo::ReturnAddress(return_address));

        // Stop if the return address was in the entry point function
        if entry_point_address_range.contains(&return_address) {
            break;
        }

        let frame_pointer_address = frame_pointer
            .checked_add_signed(offsets.frame_pointer)
            .ok_or(FramePointerUnwindError::AddressOverflow)?;
        frame_pointer =
            read_mem(core, frame_pointer_address).map_err(FramePointerUnwindError::ReadMemory)?;
    }

    Ok(stack_frames.into_iter().rev().collect())
}
