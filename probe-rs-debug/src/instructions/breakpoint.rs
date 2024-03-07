use super::{
    super::{debug_info::canonical_unit_path_eq, DebugError, DebugInfo},
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
        for program_unit in &debug_info.unit_infos {
            let Some(ref line_program) = program_unit.unit.line_program else {
                // Not all compilation units need to have debug line information, so we skip those.
                continue;
            };

            let mut num_files = line_program.header().file_names().len();

            // For DWARF version 5, the current compilation file is included in the file names, with index 0.
            //
            // For earlier versions, the current compilation file is not included in the file names, but index 0 still refers to it.
            // To get the correct number of files, we have to add 1 here.
            if program_unit.unit.header.version() <= 4 {
                num_files += 1;
            }

            // There can be multiple file indices which match, due to the inclusion of the current compilation file with index 0.
            //
            // At least for DWARF 4 there are cases where the current compilation file is also included in the file names with
            // a non-zero index.
            let matching_file_indices: Vec<_> = (0..num_files)
                .filter_map(|file_index| {
                    let file_index = file_index as u64;

                    debug_info
                        .get_path(&program_unit.unit, file_index)
                        .and_then(|combined_path: TypedPathBuf| {
                            if canonical_unit_path_eq(combined_path.to_path(), path) {
                                tracing::debug!(
                                    "Found matching file index: {file_index} for path: {path}",
                                    file_index = file_index,
                                    path = path.display()
                                );
                                Some(file_index)
                            } else {
                                None
                            }
                        })
                })
                .collect();

            if matching_file_indices.is_empty() {
                continue;
            }

            let Ok((complete_line_program, line_sequences)) = line_program.clone().sequences()
            else {
                tracing::debug!("Failed to get line sequences for line program");
                continue;
            };

            for line_sequence in line_sequences {
                let sequence = Sequence::from_line_sequence(
                    debug_info,
                    program_unit,
                    &complete_line_program,
                    &line_sequence,
                )?;

                for matching_file_index in &matching_file_indices {
                    if let Some(verified_breakpoint) =
                        sequence.haltpoint_near_source_location(Some(*matching_file_index), line, column)
                    {
                        return Ok(verified_breakpoint);
                    }
                }
            }
        }
        // If we get here, we have not found a valid breakpoint location.
        Err(DebugError::Other(format!(
            "No valid breakpoint information found for file: {}, line: {line:?}, column: {column:?}",
            path.display()
        )))
    }
}
