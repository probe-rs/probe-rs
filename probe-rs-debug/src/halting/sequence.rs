use super::{
    super::{debug_info::GimliReader, unit_info::UnitInfo, ColumnType, DebugError, DebugInfo},
    block::Block,
    instruction::Instruction,
    SourceLocation, VerifiedBreakpoint,
};
use gimli::LineSequence;
use std::{
    fmt::{Debug, Formatter},
    num::NonZeroU64,
    ops::Range,
};
use typed_path::TypedPathBuf;

/// Keep track of all the instruction locations required to satisfy the operations of [`SteppingMode`][s].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with what DWARF terms as "recommended breakpoint location".
///
/// [s]: crate::debug_step::SteppingMode
pub(crate) struct Sequence<'debug_info> {
    /// The `address_range.start` is the starting address of the program counter for which this sequence is valid,
    /// and allows us to identify target instruction locations where the program counter lies inside the prologue.
    /// The `address_range.end` is the first address that is not covered by this sequence within the line number program,
    /// and allows us to identify when stepping over a instruction location would result in leaving a sequence.
    /// - This is typically the instruction address of the first instruction in the next sequence,
    ///   which may also be the first instruction in a new function.
    pub(crate) address_range: Range<u64>,
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
            if let Some(follows) = block.stepped_from {
                write!(f, " Stepped From: {follows:#010x}")?;
            } else {
                write!(f, " Stepped From: <unknown>")?;
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
            let message = "The specified source location does not have any line_program information available. Please consider using instruction level stepping.".to_string();
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
            let message = "The specified source location does not have any line information available. Please consider using instruction level stepping.".to_string();
            return Err(DebugError::WarnAndContinue { message });
        };
        let sequence = Self::from_line_sequence(
            debug_info,
            program_unit,
            &complete_line_program,
            line_sequence,
        )?;

        if sequence.len() == 0 {
            let message = "Could not find valid instruction locations for this address. Consider using instruction level stepping.".to_string();
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
        let mut previous_block: Option<Block> = None;
        while let Some(instruction) = block_instructions.peek() {
            // Determine if these two blocks need to be connected by their edges.
            let stepped_from = previous_block.as_ref().and_then(|prev_block: &Block| {
                if prev_block
                    .steps_to
                    .map(|address| address == instruction.address)
                    .unwrap_or(false)
                {
                    prev_block.instructions.last().map(|i| i.address)
                } else {
                    None
                }
            });
            let current_block = Block::new(
                instruction.address,
                stepped_from,
                block_instructions,
                debug_info,
                program_unit,
            )?;
            previous_block = Some(current_block.clone());
            self.blocks.push(current_block);
        }
        Ok(())
    }

    /// Get the number of instruction locations in the list.
    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
    }

    /// See [`VerifiedBreakpoint::for_address()`].
    // TODO: We need tests for the various scenarios below.
    pub(crate) fn haltpoint_near_address(&self, address: u64) -> Option<VerifiedBreakpoint> {
        tracing::debug!("Looking for halt instruction at address={address:#010x}");

        let Some(block) = self
            .blocks
            .iter()
            .find(|block| block.contains_address(address))
        else {
            tracing::warn!("Could not find a valid breakpoint for address={address:#010x}");
            return None;
        };

        // Cycle through increasing degrees of "looseness" in the search for the halt instruction.
        let halt_instruction = if let Some(instruction) =
            block.instructions.iter().find(|instruction| {
                instruction.address >= address && instruction.role.is_halt_location()
            }) {
            // We found a matching halt location in the current block.
            Some(instruction)
        } else {
            // Look for the next halt instruction in any blocks that we know are linked.
            let mut halt_instruction = None;
            let mut linked_address = block.steps_to;
            while let Some(linked_block) = self.blocks.iter().find(|next_block| {
                linked_address.is_some() && next_block.stepped_from == linked_address
            }) {
                linked_address = linked_block.steps_to;
                if let Some(instruction) = linked_block.instructions.iter().find(|instruction| {
                    instruction.address >= address && instruction.role.is_halt_location()
                }) {
                    halt_instruction = Some(instruction);
                    break;
                }
            }
            halt_instruction
        };

        if let Some(breakpoint) = halt_instruction.and_then(|instruction| {
            SourceLocation::from_instruction(self.debug_info, self.program_unit, instruction).map(
                |source_location| VerifiedBreakpoint {
                    address: instruction.address,
                    source_location,
                },
            )
        }) {
            tracing::debug!("Found a matching breakpoint: {breakpoint:?}");
            Some(breakpoint)
        } else {
            tracing::warn!("Could not find a valid breakpoint for address={address:#010x}");
            None
        }
    }

    /// See [`VerifiedBreakpoint::for_source_location()`].
    // TODO: We need tests for the various scenarios below.
    pub(crate) fn haltpoint_near_source_location(
        &self,
        matching_file_index: Option<u64>,
        line: u64,
        column: Option<u64>,
    ) -> Option<VerifiedBreakpoint> {
        tracing::debug!(
            "Looking for a breakpoint for line={line}, column={} in file: {}",
            column.unwrap(),
            self.debug_info
                .get_path(&self.program_unit.unit, matching_file_index.unwrap())
                .unwrap()
                .to_string_lossy()
        );

        // First, let's reduce the blocks to only those that contain the file index we are looking for.
        // We do this, because in real life, users are more like to request the file and line,
        // but cannot accurately specify the column that contains a valid halt location. The result is
        // that trying to do an exact file+line+column match as the first step is likely to useless.
        let matching_blocks = self
            .blocks
            .iter()
            .filter(|block| {
                block
                    .instructions
                    .iter()
                    .any(|instruction| matching_file_index == Some(instruction.file_index))
            })
            .collect::<Vec<&Block>>();

        // Cycle through various degrees of matching, to find the most relevant source location.
        // We have to do this in multiple iterations because instructions are allocated to blocks
        // based on their instruction address, and not their source location.
        for block in &matching_blocks {
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
                .and_then(|matching_location| {
                    self.haltpoint_near_address(matching_location.address)
                })
            {
                tracing::debug!("Found a closely matching breakpoint: {matching_breakpoint:?}");
                return Some(matching_breakpoint);
            }
        }

        // If we still haven't found a halt instruction, then we try to find the next
        // and closest source line, in the same file. This is a bit risky, because
        // those lines may not be part of the same branch of execution. That said,
        // the process of setting breakpoints by source location is usually a
        // visual process, with feedback. e.g. in VSCode, the actual breakpoint location
        // is shown in the editor, and the user can see if it represents a reasonable alternative.
        // This is how GDB does it also.
        let mut sorted_file_lines = Vec::new();
        for block in matching_blocks {
            for instruction in &block.instructions {
                if matching_file_index == Some(instruction.file_index) {
                    sorted_file_lines.push(instruction);
                }
            }
        }
        sorted_file_lines.sort_by(|a, b| a.line.cmp(&b.line));

        for matching_location in sorted_file_lines {
            if matching_location.line > NonZeroU64::new(line) {
                if let Some(matching_breakpoint) =
                    self.haltpoint_near_address(matching_location.address)
                {
                    tracing::warn!(
                        "Suggesting an closely matching breakpoint: {matching_breakpoint:?}"
                    );
                    return Some(matching_breakpoint);
                }
            }
        }

        tracing::warn!(
            "Could not find a valid breakpoint for line={line}, column={} in file: {}",
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
