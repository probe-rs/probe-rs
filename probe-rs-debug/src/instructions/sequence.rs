use super::{
    super::{debug_info::GimliReader, function_die::FunctionDie, unit_info::UnitInfo, DebugError, DebugInfo},
    block::Block,
    instruction::{self, Instruction},
};
use gimli::LineSequence;
use std::{
    fmt::{Debug, Formatter},
    ops::Range,
};
use typed_path::TypedPathBuf;

/// Keep track of all the instruction locations required to satisfy the operations of [`SteppingMode`][s].
/// This is a list of target instructions, belonging to a [`gimli::LineSequence`],
/// and filters it to only user code instructions (no prologue code, and no non-statement instructions),
/// so that we are left only with what DWARF terms as "recommended breakpoint location".
///
/// [s]: crate::debug::debug_step::SteppingMode
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
                    block.function_name
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

        let sequence_function = program_unit
            .get_function_dies(debug_info, sequence.address_range.start)
            .map(|function_dies| function_dies.first().cloned())?;

        // Now that we have all the instructions, we can create the blocks.
        let first_block = sequence.build_blocks(
            debug_info,
            program_unit,
            &mut sequence_instructions.iter().peekable(),
            sequence_function,
        )?;
        sequence.blocks.push(first_block);
        // Sort the blocks to comply with the DWARF specification, 6.2.5. (they get muddled because of the recursion).
        sequence.blocks.sort_by_key(|block| {
            block
                .instructions
                .first()
                .map(|instruction| instruction.address)
        });
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
        block_function: Option<FunctionDie<'debug_info>>,
    ) -> Result<Block, DebugError> {
        let mut block = Block {
            function_name: block_function
                .as_ref()
                .and_then(|block_function| block_function.function_name(debug_info))
                .unwrap_or_else(|| "<unknown>".to_string()),
            is_inlined: block_function
                .as_ref()
                .map(|block_function| block_function.is_inline())
                .unwrap_or(false),
            instructions: Vec::new(),
            stepped_from: None,
            steps_to: None,
        };
        while let Some(instruction) = block_instructions.next() {
            let next_instruction = block_instructions.peek().cloned();
            // Break out the prologue block, and recurse through the remaining instructions.
            if instruction.role == instruction::InstructionRole::Prologue
                && next_instruction
                    .map(|ni| ni.role != instruction::InstructionRole::Prologue)
                    .unwrap_or(true)
            {
                self.recurse_inner_block(
                    &mut block,
                    Some(instruction.address),
                    debug_info,
                    program_unit,
                    block_instructions,
                    block_function.clone(),
                    instruction,
                )?;
                block.steps_to = next_instruction.map(|ni| ni.address);
                continue;
            // Check if we're on the final instruction before returning from an inlined function.
            } else if block.is_inlined
                && block_function
                    .as_ref()
                    .map(|block_function| {
                        next_instruction.map(|ni| ni.address) == block_function.high_pc()
                    })
                    .unwrap_or(false)
            {
                // Inlined instructions immediately precede the call site.
                block.steps_to = next_instruction.map(|ni| ni.address);
                block.instructions.push(*instruction);
                break;
            }
            // Check if we're about to step into an inlined function, and recurse ...
            else if next_instruction.is_some() {
                let inlined_function = next_instruction
                    .and_then(|ni| {
                        program_unit
                            .get_function_dies(debug_info, ni.address)
                            .map(|function_dies| function_dies.last().cloned())
                            .ok()
                    })
                    .flatten();
                if inlined_function != block_function {
                    self.recurse_inner_block(
                        &mut block,
                        None,
                        debug_info,
                        program_unit,
                        block_instructions,
                        inlined_function,
                        instruction,
                    )?;
                    // This instruction will be in a separate block from the inline instructions.
                    continue;
                }
            }
            // If this instruction is not a boundary, then simply add it to the current block.
            block.instructions.push(*instruction);
        }

        Ok(block)
    }

    /// The steps to recurse, and return from an inner block to the outer block.
    // Allowing too many arguments, because repeating all this code inside `fn build_blocks` would be worse.
    #[allow(clippy::too_many_arguments)]
    fn recurse_inner_block(
        &mut self,
        block: &mut Block,
        edge_address: Option<u64>,
        debug_info: &'debug_info DebugInfo,
        program_unit: &'debug_info UnitInfo,
        block_instructions: &mut std::iter::Peekable<std::slice::Iter<'_, Instruction>>,
        inlined_function: Option<FunctionDie<'debug_info>>,
        instruction: &Instruction,
    ) -> Result<(), DebugError> {
        let mut inner_block = self.build_blocks(
            debug_info,
            program_unit,
            block_instructions,
            inlined_function,
        )?;
        inner_block.stepped_from = edge_address;
        self.blocks.push(inner_block);
        // We can move on the next instruction, as we've already processed the current one.
        block.instructions.push(*instruction);
        Ok(())
    }

    /// Get the number of instruction locations in the list.
    pub(crate) fn len(&self) -> usize {
        self.blocks.len()
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
