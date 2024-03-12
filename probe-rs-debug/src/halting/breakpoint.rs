use std::num::NonZeroU64;

use super::{
    super::{DebugError, DebugInfo},
    instruction::Instruction,
    line_sequences_for_path,
    sequence::Sequence,
    SourceLocation,
};
use typed_path::{TypedPath, TypedPathBuf};

/// A verified breakpoint represents an instruction address, and the source location that it corresponds to it,
/// for locations in the target binary that comply with the DWARF standard terminology for "recommended breakpoint location".
/// This typically refers to instructions that are not part of the prologue or epilogue, and are part of the user code,
/// or are the final instruction in a sequence, before the processor begins the epilogue code.
/// The `probe-rs` debugger uses this information to identify valid halt locations for breakpoints and stepping.
#[derive(Clone, Debug)]
pub struct VerifiedBreakpoint {
    /// The address in target memory, where the breakpoint can be set.
    pub address: u64,
    /// If the breakpoint request was for a specific source location, then this field will contain the resolved source location.
    pub source_location: SourceLocation,
}

impl VerifiedBreakpoint {
    /// Return the first valid breakpoint location of the statement that is greater than OR equal to `address`.
    /// e.g., if the `address` is the current program counter, then the return value will be the next valid halt address
    /// in the current sequence, where the address is part of the sequence, and will NOT be bypassed because of branching.
    pub(crate) fn for_address(
        debug_info: &DebugInfo,
        address: u64,
    ) -> Result<VerifiedBreakpoint, DebugError> {
        let sequence = Sequence::from_address(debug_info, address)?;

        if let Some(verified_breakpoint) = sequence.haltpoint_near_address(address) {
            tracing::debug!(
                "Found valid breakpoint for address: {:#010x} : {verified_breakpoint:?}",
                &address
            );
            return Ok(verified_breakpoint);
        }
        // If we get here, we have not found a valid breakpoint location.
        let message = format!(
            "Could not identify a valid breakpoint for address: {address:#010x}. Please consider using instruction level stepping."
        );
        Err(DebugError::WarnAndContinue { message })
    }

    /// Identifying the breakpoint location for a specific location (path, line, column) is a bit more complex,
    /// compared to the `for_address()` method, due to a few factors:
    /// - The correct program instructions, may be in any of the compilation units of the current program.
    /// - The debug information may not contain data for the "specific source" location requested:
    ///   - DWARFv5 standard, section 6.2, allows omissions based on certain conditions. In this case,
    ///     we need to find the closest "relevant" source location that has valid debug information.
    ///   - The requested location may not be a valid source location, e.g. when the
    ///     debug information has been optimized away. In this case we will return an appropriate error.
    ///
    /// #### The logic used to find the "most relevant" source location is as follows:
    /// - Filter  [`UnitInfo`] , by using [`gimli::LineProgramHeader`] to match units that include the requested path.
    /// - For each matching compilation unit, get the [`gimli::LineProgram`] and [`Vec<LineSequence>`][gimli::LineSequence].
    /// - Convert [`LineSequence`][gimli::LineSequence], to [`Sequence`] to infer statement block boundaries.
    /// - Return the first `Instruction` that contains the requested source location, being one of the following:
    ///   - This may be an exact match on file/line/column, or,
    ///   - Failing an exact match, a match on file/line only.
    ///   - Failing that, a match on file only, where the line number is the "next" available instruction,
    ///     on the next available line of the specified file.
    pub(crate) fn for_source_location(
        debug_info: &DebugInfo,
        path: TypedPath,
        line: u64,
        column: Option<u64>,
    ) -> Result<Self, DebugError> {
        // Keep track of the matching file index to avoid having to lookup and match the full path
        // for every row in the program line sequence.
        let path_buf = TypedPathBuf::from(path.as_bytes());
        let line_sequences = line_sequences_for_path(debug_info, &path_buf);
        for (sequence, matching_file_index) in &line_sequences {
            if let Some(verified_breakpoint) =
                sequence.haltpoint_near_source_location(*matching_file_index, line, column)
            {
                return Ok(verified_breakpoint);
            }
        }
        // If we get here, we need a "best effort" approach to find the next line in the file with a valid haltpoint.
        if let Some(verified_breakpoint) =
            VerifiedBreakpoint::for_next_line_after_line(debug_info, &line_sequences, line)
        {
            return Ok(verified_breakpoint);
        }

        // If we get here, we have not found a valid breakpoint location.
        Err(DebugError::Other(format!(
            "No valid breakpoint information found for file: {}, line: {line:?}, column: {column:?}",
            path.display()
        )))
    }

    fn for_next_line_after_line(
        debug_info: &DebugInfo,
        file_sequences: &[(Sequence, Option<u64>)],
        line: u64,
    ) -> Option<Self> {
        let mut sorted_haltpoints: Vec<&Instruction> = Vec::new();
        for file_sequence in file_sequences {
            let (sequence, file_index) = file_sequence;
            sorted_haltpoints.extend(sequence.blocks.iter().flat_map(|block| {
                block.instructions.iter().filter(|instruction| {
                    file_index
                        .map(|index| {
                            instruction.role.is_halt_location()
                                && instruction.file_index == index
                                && instruction.line >= NonZeroU64::new(line)
                        })
                        .unwrap_or(false)
                })
            }));
        }
        sorted_haltpoints.sort_by_key(|instruction| instruction.line);
        if let Some(matching_breakpoint) = sorted_haltpoints
            .iter()
            .find(|instruction| instruction.line > NonZeroU64::new(line))
            .and_then(|instruction| {
                VerifiedBreakpoint::for_address(debug_info, instruction.address).ok()
            })
        {
            tracing::warn!("Suggesting a closely matching breakpoint: {matching_breakpoint:?}");
            Some(matching_breakpoint)
        } else {
            None
        }
    }
}
