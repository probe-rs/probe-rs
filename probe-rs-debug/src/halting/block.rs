use super::{
    super::{unit_info::UnitInfo, DebugError, DebugInfo},
    instruction::{Instruction, InstructionRole},
};
use std::ops::RangeInclusive;

/// The concept of an instruction block is based on
/// [Rust's MIR basic block definition](https://rustc-dev-guide.rust-lang.org/appendix/background.html#cfg)
/// The concept is also a close match for how the DAP specification defines the a `statement`
/// [SteppingGranularity](https://microsoft.github.io/debug-adapter-protocol/specification#Types_SteppingGranularity)
/// In the context of the `probe-rs` debugger, an instruction block is a contiguous series of instructions
/// which belong to a single [`Sequence`].
/// The key difference between instructions in a block, and those in a [`gimli::LineSequence`], is that we can rely
/// on the 'next' instruction in the block to be the 'next' instruction the processor will execute (barring any interrupts).
/// ### Implementation discussion:
/// Indentifying the boundaries of each [`Block`] is the key to identifying valid halt locations, and is the primary
/// purpose of the [`Block`] struct. Current versions of Rust (up to rustc 1.76.0) does not populate the
/// `DW_LNS_basic_block` attribute of the line program rows in the DWARF debug information. The implication of this is that
/// we need to infer the boundaries of each block withing the sequence of instructions, from other blocks, as well as
/// from the prologue and epilogue markers. The approach taken is as follows:
/// - To facilitate 'stepping', we also need to identify how blocks transition from one to the next,
///   and unlike inside a sequence, these are typically not sequential addresses. The `stepped_from` and `steps_to`
///   fields are used to identify the addresses of the instructions that are the left and right edges of the block.
///   The DWARF line program rows do not have enough information to identify branching instructions, and so we
///   cannot rely on the sequence of instructions in a line program sequence to identify the block boundaries.
///   To avoid having to interpret the Assembly instructions for every architecture, we use some basic heuristics
///   to identify block boundaries. Some of these can be inferred from the DWARF debug information, while others
///   can only be assessed using information about the stackframes in an unwinding context.
/// - The DWARF based heuristics used to identify block boundaries are as follows:
///   - The first block is the prologue block, and is identified by the `DW_LNS_set_prologue_end` attribute on the first
///   - first insruction after the prologue.
///   - The `DW_NLS_epilogue_begin` instructions are always in their own block, and linked with preceding blocks
///     based on available line and column information.
///   - The first block after the prologue, steps directly from the prologue block.
///   - Inlined code (functions or macros) always precede the instruction that called them. They have their
///     own blocks, and the last one is linked to the instruction that called them.
///   - If a function/sequence has multiple ranges, then the instructions in those ranges are assumed to be
///     divergent in some way, and are therefore in separate blocks.
///   - The remaining instructions are grouped into blocks containing the contiguous instructions belonging to the same
///     source file line.
#[derive(Clone, Default)]
pub(crate) struct Block {
    /// This block contains instructions that was inlined (function or macro) into the current sequence.
    pub(crate) is_inlined: bool,
    pub(crate) instructions: Vec<Instruction>,
    ///  - The `steps_to` identifies the address of the instruction immediately following this block.
    pub(crate) steps_to: Option<u64>,
}

impl Block {
    pub(crate) fn new(
        starting_address: u64,
        block_instructions: &mut std::iter::Peekable<std::slice::Iter<Instruction>>,
        debug_info: &DebugInfo,
        program_unit: &UnitInfo,
    ) -> Result<Self, DebugError> {
        let block_function = program_unit
            .get_function_dies(debug_info, starting_address)
            .map(|function_dies| function_dies.last().cloned())?;
        let mut block = Block {
            is_inlined: block_function
                .as_ref()
                .map(|block_function| block_function.is_inline())
                .unwrap_or(false),
            instructions: Vec::new(),
            steps_to: None,
        };
        while let Some(instruction) = block_instructions.next() {
            let next_instruction = block_instructions.peek().cloned();

            // Some of the blocks below have the same logic, but the conditions are complex enough
            // that it is worth keeping them separate.
            #[allow(clippy::if_same_then_else)]
            // End the prologue block, if the next instruction is not a prologue instruction.
            if instruction.role == InstructionRole::Prologue
                && next_instruction
                    .map(|ni| ni.role != InstructionRole::Prologue)
                    .unwrap_or(true)
            {
                block.instructions.push(*instruction);
                block.steps_to = next_instruction.map(|ni| ni.address);
                break;
            }
            // End the block, if the next instruction is the beginning of the epilogue.
            else if next_instruction
                .map(|ni| ni.role == InstructionRole::EpilogueBegin)
                .unwrap_or(true)
            {
                block.instructions.push(*instruction);
                block.steps_to = next_instruction.map(|ni| ni.address);
                break;
            }
            // End the current block, if we're on the final instruction before returning from an inlined function.
            else if block.is_inlined
                && block_function
                    .as_ref()
                    .map(|block_function| {
                        next_instruction.map(|ni| ni.address) == block_function.high_pc()
                    })
                    .unwrap_or(false)
            {
                // Inlined instructions immediately precede the call site.
                block.instructions.push(*instruction);
                block.steps_to = next_instruction.map(|ni| ni.address);
                break;
            }
            // End the current block, if we're about to step into an inlined function.
            else if block_function.is_some()
                && block_function
                    != next_instruction
                        .and_then(|ni| {
                            program_unit
                                .get_function_dies(debug_info, ni.address)
                                .map(|function_dies| function_dies.last().cloned())
                                .ok()
                        })
                        .flatten()
            {
                block.instructions.push(*instruction);
                block.steps_to = next_instruction.map(|ni| ni.address);
                break;
            }
            // When we're not at one of the known boundaries, then we end blocks to conservatively to avoid
            // false assumptions about whether two instructions belong in the same block.
            // Break between instructions that are not in the same file, or not on the same line.
            else if next_instruction
                .map(|ni| {
                    (ni.file_index != instruction.file_index || ni.line != instruction.line)
                        && (instruction.role == InstructionRole::HaltPoint
                            || instruction.role == InstructionRole::Other)
                        && ni.role == InstructionRole::HaltPoint
                })
                .unwrap_or(false)
            {
                // The next instruction is ...
                block.instructions.push(*instruction);
                break;
            }
            // Finally, if this instruction is not deemed part of one of the above boundary conditions,
            // then simply add it to the current block.
            block.instructions.push(*instruction);
        }
        Ok(block)
    }

    /// The range of addresses that the block covers is 'inclusive' on both ends.
    pub(crate) fn included_addresses(&self) -> Option<RangeInclusive<u64>> {
        self.instructions
            .first()
            .map(|first| &first.address)
            .and_then(|first| self.instructions.last().map(|last| *first..=last.address))
    }

    /// Check if the block contains the address.
    pub(crate) fn contains_address(&self, address: u64) -> bool {
        self.included_addresses()
            .map(|included_addresses| included_addresses.contains(&address))
            .unwrap_or(false)
    }
}
