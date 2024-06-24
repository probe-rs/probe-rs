use super::{
    super::{debug_info::GimliReader, unit_info::UnitInfo, ColumnType, DebugError, DebugInfo},
    block::Block,
    instruction::{Instruction, InstructionRole},
    SourceLocation,
};
use gimli::LineSequence;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
    ops::Range,
};
use typed_path::TypedPathBuf;

/// Keep track of all the instruction locations required to satisfy the operations of [`Stepping`][s].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with what DWARF terms as "recommended breakpoint location".
///
/// [s]: super::stepping::Stepping
pub(crate) struct Sequence<'debug_info> {
    /// The `address_range.start` is the starting address of the program counter for which this sequence is valid,
    /// and allows us to identify target instruction locations where the program counter lies inside the prologue.
    /// The `address_range.end` is the first address that is not covered by this sequence within the line number program,
    /// and allows us to identify when stepping over a instruction location would result in leaving a sequence.
    /// - This is typically the instruction address of the first instruction in the next sequence,
    ///   which may also be the first instruction in a new function.
    pub(crate) address_range: Range<u64>,
    /// Identify the last valid halt location in the sequence. This is not the same as the
    /// start of epilogue, which may occur more than once in a sequence.
    pub(crate) last_halt_instruction: Option<u64>,
    /// See [`Block`].
    /// Note: The process of recursing the line sequence to create blocks,
    /// is likely to create blocks that our out of sequence, so we sort them to
    /// comply with the DWARF specification, 6.2.5 to ensure the addresses in
    /// the sequence are monotonically increasing. This does not affect the stepping,
    /// because we do not (and should not) rely on the order of the blocks to step through the sequence.
    pub(crate) blocks: Vec<Block>,
    /// Required to resolve information about function calls, etc.
    pub(crate) debug_info: &'debug_info DebugInfo,
    /// Required to resolve information about function calls, etc.
    pub(crate) program_unit: &'debug_info UnitInfo,
}

impl PartialEq for Sequence<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.address_range == other.address_range
    }
}

impl Debug for Sequence<'_> {
    /// We implement a single Debug for the sequence, its blocks, and the instructions in each block,
    /// so that we don't have to store references to `DebugInfo` and `UnitInfo` in the `Block` and `Instruction` types.
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "Sequence range: {:#010x}..{:#010x}",
            self.address_range.start, self.address_range.end
        )?;
        for block in &self.blocks {
            if let Some(included_addresses) = block.included_addresses() {
                write!(
                    f,
                    "  Block range: {:#010x}..={:#010x}. {}Function: {}",
                    included_addresses.start(),
                    included_addresses.end(),
                    if block.is_inlined { "Inlined " } else { "" },
                    self.program_unit
                        .get_function_dies(self.debug_info, *included_addresses.start())
                        .map(|function_dies| function_dies.last().cloned())
                        .ok()
                        .and_then(|function_die| function_die
                            .and_then(|function_die| function_die.function_name(self.debug_info)))
                        .unwrap_or("unknown".to_string()),
                )?;
            } else {
                write!(f, "  Block range: <empty>")?;
            }
            if let Some(precedes) = block.steps_to {
                write!(f, " Steps To: {precedes:#010x}")?;
            } else {
                write!(f, " Steps To: <unknown>")?;
            }
            writeln!(f)?;
            for instruction in &block.instructions {
                writeln!(
                    f,
                    "    {instruction:?} - {:?}",
                    self.debug_info
                        .get_path(&self.program_unit.unit, instruction.file_index)
                        .map(
                            |file_path| TypedPathBuf::from_unix(file_path.file_name().unwrap())
                                .to_string_lossy()
                                .to_string()
                        )
                        .unwrap_or("<unknown file>".to_string())
                )?;
            }
            writeln!(f)?;
        }
        Ok(())
    }
}

impl<'debug_info> Sequence<'debug_info> {
    /// Extract all the instruction locations, belonging to the active sequence (i.e. the sequence that contains the `address`).
    pub(crate) fn from_address(
        debug_info: &'debug_info DebugInfo,
        program_counter: u64,
    ) -> Result<Self, DebugError> {
        let program_unit = debug_info.compile_unit_info(program_counter)?;
        let (offset, address_size) = if let Some(line_program) =
            program_unit.unit.line_program.clone()
        {
            (
                line_program.header().offset(),
                line_program.header().address_size(),
            )
        } else {
            let message = "The specified source location does not have any line_program information available.".to_string();
            return Err(DebugError::WarnAndContinue { message });
        };

        // Get the sequences of rows from the CompleteLineProgram at the given program_counter.
        let incomplete_line_program =
            debug_info
                .debug_line_section
                .program(offset, address_size, None, None)?;
        let (complete_line_program, line_sequences) = incomplete_line_program.sequences()?;

        // Get the sequence of rows that belongs to the program_counter.
        let Some(line_sequence) = line_sequences.iter().find(|line_sequence| {
            line_sequence.start <= program_counter && program_counter < line_sequence.end
        }) else {
            let message =
                "The specified source location does not have any line information available."
                    .to_string();
            return Err(DebugError::WarnAndContinue { message });
        };
        let sequence = Self::from_line_sequence(
            debug_info,
            program_unit,
            &complete_line_program,
            line_sequence,
        )?;

        if sequence.len() == 0 {
            let message =
                "Could not find valid instruction locations for this address.".to_string();
            Err(DebugError::WarnAndContinue { message })
        } else {
            tracing::trace!(
                "Instruction location for pc={:#010x}\n{:?}",
                program_counter,
                sequence
            );
            Ok(sequence)
        }
    }

    /// Build [`Sequence`] from a [`gimli::LineSequence`], with all the markers we need to determine valid halt locations.
    pub(crate) fn from_line_sequence(
        debug_info: &'debug_info DebugInfo,
        program_unit: &'debug_info UnitInfo,
        complete_line_program: &gimli::CompleteLineProgram<GimliReader>,
        line_sequence: &LineSequence<GimliReader>,
    ) -> Result<Self, DebugError> {
        let program_language = program_unit.get_language();
        let mut sequence_rows = complete_line_program.resume_from(line_sequence);

        // We have enough information to create the Sequence.
        let mut sequence = Sequence {
            address_range: line_sequence.start..line_sequence.end,
            last_halt_instruction: None,
            blocks: Vec::new(),
            debug_info,
            program_unit,
        };

        // Temporarily collect all the instructions in the sequence, before we re-process them to create the blocks.
        let mut sequence_instructions: Vec<Instruction> = Vec::new();
        let mut prologue_completed = false;
        let mut previous_row: Option<gimli::LineRow> = None;

        while let Ok(Some((_, row))) = sequence_rows.next_row() {
            if !prologue_completed && is_prologue_complete(row, program_language, previous_row) {
                // This is the first row after the prologue.
                prologue_completed = true;
            }

            if !prologue_completed {
                log_row_eval(line_sequence, row, "  inside prologue>");
            } else {
                log_row_eval(line_sequence, row, "  after prologue>");
            }

            // The end of the sequence is not a valid halt location,
            // nor is it a valid instruction in the current sequence.
            if row.end_sequence() {
                break;
            }

            // We need to know the last halt location in the sequence,
            // and since we are already iterating through the rows, we can do it here,
            // instead of iterating through the instructions again during runtime.
            if row.is_stmt() || row.epilogue_begin() {
                sequence.last_halt_instruction = Some(row.address());
            }

            sequence_instructions.push(Instruction::from_line_row(
                prologue_completed,
                row,
                previous_row.as_ref(),
            ));
            previous_row = Some(*row);
        }

        // Now that we have all the instructions, we can create the blocks.
        sequence.build_blocks(
            debug_info,
            program_unit,
            &mut sequence_instructions.iter().peekable(),
        )?;

        //TODO: Create a test to compare the number of instructions in the sequence with the number of instructions in the blocks.
        tracing::trace!(
            "The `Sequence` has {} instructions, and {} blocks.",
            sequence_instructions.len(),
            sequence.blocks.len(),
        );
        tracing::trace!(
            "\tThe blocks combined have a total of {} instructions",
            sequence
                .blocks
                .iter()
                .map(|block| block.instructions.len())
                .sum::<usize>()
        );
        tracing::trace!("{sequence:?}");
        Ok(sequence)
    }

    /// Process instructions into blocks, based on their definition,
    /// position in the sequence, and other debug information.
    /// Returns the address of the last instruction in the block.
    fn build_blocks(
        &mut self,
        debug_info: &'debug_info DebugInfo,
        program_unit: &'debug_info UnitInfo,
        block_instructions: &mut std::iter::Peekable<std::slice::Iter<Instruction>>,
    ) -> Result<(), DebugError> {
        while let Some(instruction) = block_instructions.peek() {
            let current_block = Block::new(
                instruction.address,
                block_instructions,
                debug_info,
                program_unit,
            )?;
            self.blocks.push(current_block);
        }
        Ok(())
    }

    /// Get the number of instruction locations in the list.
    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }

    /// See [`SourceLocation::breakpoint_at_address()`].
    /// Note: The sequence was created for the same address, so we know the address lives
    /// in this range.
    /// - When we have an exact match on a known instruction, we will return it.
    /// - If the address lies in the prologue of a sequence, we will return the
    ///   first halt location in the sequence.
    /// - If the address lies between known instruction addresses then we will attempt to find
    ///   the "closest preceding halt location address".
    ///   - This will be done conservatively, constraining the result to halt locations that
    ///     are known to be part of the same sequence, and which will not be bypassed because
    ///     of branching inside the sequence.
    ///   - If this is not possible, we will return `None`, rather than mislead the calling code.
    pub(crate) fn haltpoint_for_address(&self, address: u64) -> Option<SourceLocation> {
        tracing::trace!("Looking for halt instruction at address={address:#010x}");

        let mut halt_instruction = None;

        // Cycle through increasing degrees of "looseness" in the search for the halt instruction.

        // First look for blocks that contain the address.
        if let Some(block) = self
            .blocks
            .iter()
            .find(|block| block.contains_address(address))
        {
            // Try a match on a known "halt friendly" instruction.
            if let Some(instruction) = block.instructions.iter().find(|instruction| {
                instruction.address == address && instruction.role.is_halt_location()
            }) {
                tracing::trace!("Found match for instruction @{address:#010x}, in: {self:?}");
                halt_instruction = Some(instruction);
            }

            // - If the address is in the prologue, use the first (post prologue) halt location in the sequence.
            //   - The first (post prologue) halt location may be in the next (steps_to) block.
            // - If the block contains the address, but there is no exact match, or
            //   if the block contains the address and there is an exact match which is not
            //   a halting address, then we will use the previous halt instruction.
            if halt_instruction.is_none() {
                let prologue_check = block.instructions.iter();
                let mut last_known_halt_instruction = None;
                for (position, instruction) in prologue_check.enumerate() {
                    if instruction.role == InstructionRole::PrologueHaltPoint {
                        if position + 1 == block.instructions.len() {
                            if let Some(step_to_address) = block.steps_to {
                                // The prologue is the last instruction in the block.
                                return self.haltpoint_for_address(step_to_address);
                            } else {
                                // We have no way of knowing where this steps to.
                                break;
                            }
                        }
                        continue;
                    } else if instruction.role.is_halt_location() {
                        if address <= instruction.address {
                            last_known_halt_instruction = Some(instruction);
                            continue;
                        } else if address > instruction.address
                            && last_known_halt_instruction.is_some()
                        {
                            // The address was between two halt locations.
                            break;
                        } else {
                            // The address was in the prologue.
                            halt_instruction = Some(instruction);
                            break;
                        }
                    }
                }
                if halt_instruction.is_none() && last_known_halt_instruction.is_some() {
                    halt_instruction = last_known_halt_instruction;
                }
            }
        };

        if halt_instruction.is_none() {
            // If there is no block that contains the address, find the last block
            // that might contain the address, and use the last instruction in that block.
            let mut blocks = self.blocks.iter().peekable();
            while let Some(block) = blocks.next() {
                if block
                    .included_addresses()
                    .map(|range| *range.end() < address)
                    .unwrap_or(false)
                    && blocks
                        .peek()
                        .map(|next_block| {
                            next_block
                                .included_addresses()
                                .map(|range| *range.start() >= address)
                                .unwrap_or(false)
                        })
                        .unwrap_or(false)
                {
                    halt_instruction = block.instructions.last();
                    break;
                }
            }
        }

        if halt_instruction.is_none() {
            // If the address is in range of the sequence (exclusive Range),
            // but after the last instruction in the last block (inclusive Range),
            // then we will use the last halt instruction in the sequence.
            if let Some(last_halt_instruction) = self.last_halt_instruction {
                if address >= last_halt_instruction {
                    return self.haltpoint_for_address(last_halt_instruction);
                }
            }
        }

        if let Some(breakpoint) = halt_instruction.and_then(|instruction| {
            SourceLocation::from_instruction(self.debug_info, self.program_unit, instruction)
        }) {
            tracing::trace!("Found a matching breakpoint: {breakpoint:?}");
            Some(breakpoint)
        } else {
            tracing::trace!(
                "No valid breakpoint for address={address:#010x}(close match), in: {self:?}"
            );
            None
        }
    }

    // TODO: We need tests for the various scenarios below.
    /// If the current instruction is in a ['Block'], find the next valid halt location in the
    /// next linked block in the sequence.
    pub(crate) fn haltpoint_for_next_block(&self, address: u64) -> Option<SourceLocation> {
        tracing::trace!("Looking for next block halt instruction at address={address:#010x}");

        let Some(block) = self
            .blocks
            .iter()
            .find(|block| block.contains_address(address))
        else {
            tracing::trace!("No valid breakpoint for address={address:#010x} in: {self:?}");
            return None;
        };

        // Cycle through increasing degrees of "looseness" in the search for the halt instruction.

        // Look for the next halt instruction in any blocks that we know are linked.
        let mut halt_instruction = None;
        let mut linked_address = block.steps_to;
        while let Some(linked_block) = self.blocks.iter().find(|next_block| {
            linked_address.is_some()
                && linked_address
                    .map(|linked_address| next_block.contains_address(linked_address))
                    .unwrap_or(false)
        }) {
            linked_address = linked_block.steps_to;
            if let Some(instruction) = linked_block.instructions.iter().find(|instruction| {
                instruction.address >= address && instruction.role.is_halt_location()
            }) {
                halt_instruction = Some(instruction);
                break;
            }
        }

        if let Some(breakpoint) = halt_instruction.and_then(|instruction| {
            SourceLocation::from_instruction(self.debug_info, self.program_unit, instruction)
        }) {
            tracing::debug!("Found a matching breakpoint: {breakpoint:?}");
            Some(breakpoint)
        } else {
            tracing::debug!("No valid breakpoint for address={address:#010x} in: {block:?}");
            None
        }
    }

    /// Find a valid haltpoint based on either the file plus line plus column, or failing that,
    /// the first available haltpoint that matches the file plus line (any colunn).
    /// See [`SourceLocation::for_source_location()`].
    // TODO: We need tests for the various scenarios below.
    pub(crate) fn haltpoint_for_source_location(
        &self,
        matching_file_index: Option<u64>,
        line: u64,
        column: Option<u64>,
    ) -> Option<SourceLocation> {
        tracing::debug!(
            "Looking for a breakpoint for line={line}, column={} in file: {}",
            column.unwrap(),
            self.debug_info
                .get_path(&self.program_unit.unit, matching_file_index.unwrap())
                .unwrap()
                .to_string_lossy()
        );
        // Cycle through various degrees of matching, to find the most relevant source location.
        // We have to do this in multiple iterations because instructions are allocated to blocks
        // based on their instruction address, and not based on their source location.
        for block in &self.blocks {
            // Try an exact match.
            if let Some(matching_breakpoint) = block
                .instructions
                .iter()
                .find(|&location| {
                    matching_file_index == Some(location.file_index)
                        && NonZeroU64::new(line) == location.line
                        && ColumnType::from(column.unwrap_or(0)) == location.column
                })
                .or_else(|| {
                    // Try without a column specifier.
                    block.instructions.iter().find(|&location| {
                        matching_file_index == Some(location.file_index)
                            && NonZeroU64::new(line) == location.line
                    })
                })
                .and_then(|matching_location| self.haltpoint_for_address(matching_location.address))
            {
                tracing::debug!("Found a closely matching breakpoint: {matching_breakpoint:?}");
                return Some(matching_breakpoint);
            }
        }

        tracing::trace!(
            "Sequence does not contain a valid breakpoint for line={line}, column={} in file: {}",
            column.unwrap(),
            self.debug_info
                .get_path(&self.program_unit.unit, matching_file_index.unwrap())
                .unwrap()
                .to_string_lossy()
        );
        None
    }
}

/// Test if the current row signals that we are beyond the prologue, and into user code
fn is_prologue_complete(
    row: &gimli::LineRow,
    program_language: gimli::DwLang,
    previous_row: Option<gimli::LineRow>,
) -> bool {
    let mut prologue_completed = row.prologue_end();

    // For GNU C, it is known that the `DW_LNS_set_prologue_end` is not set, so we employ the same heuristic as GDB to determine when the prologue is complete.
    // For other C compilers in the C99/11/17 standard, they will either set the `DW_LNS_set_prologue_end` or they will trigger this heuristic also.
    // See https://gcc.gnu.org/legacy-ml/gcc-patches/2011-03/msg02106.html
    if !prologue_completed
        && matches!(
            program_language,
            gimli::DW_LANG_C99 | gimli::DW_LANG_C11 | gimli::DW_LANG_C17
        )
    {
        if let Some(prev_row) = previous_row {
            if row.end_sequence()
                || (row.is_stmt()
                    && (row.file_index() == prev_row.file_index()
                        && (row.line() != prev_row.line() || row.line().is_none())))
            {
                prologue_completed = true;
            }
        }
    }
    prologue_completed
}

/// Helper function to avoid code duplication when logging of information during row evaluation.
fn log_row_eval(
    active_sequence: &LineSequence<GimliReader>,
    row: &gimli::LineRow,
    status: &str,
) {
    tracing::trace!(
        "Sequence: line={:04} col={:05} f={:02} stmt={:5} ep={:5} es={:5} eb={:5} : {:#010X}<={:#010X}<{:#010X} : {}",
        match row.line() {
            Some(line) => line.get(),
            None => 0,
        },
        match row.column() {
            gimli::ColumnType::LeftEdge => 0,
            gimli::ColumnType::Column(column) => column.get(),
        },
        row.file_index(),
        row.is_stmt(),
        row.prologue_end(),
        row.end_sequence(),
        row.epilogue_begin(),
        active_sequence.start,
        row.address(),
        active_sequence.end,
        status
    );
}
