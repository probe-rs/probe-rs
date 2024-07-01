mod block;
mod instruction;
mod sequence;
mod stepping;
use self::sequence::Sequence;
use super::{unit_info, ColumnType, DebugError, DebugInfo};
use core::fmt::Debug;
use instruction::Instruction;
use std::num::NonZeroU64;
pub use stepping::Stepping;
use typed_path::TypedPathBuf;

/// A specific location in source code, represented by an instruction address and source file information.
/// Not all instructions have a known (from debug info) source location.
/// Each unique address, line, column, file and directory combination is a unique source location.
#[derive(Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    /// The address of the instruction in target memory.
    pub address: u64,
    /// The line number in the source file with zero based indexing.
    pub line: Option<u64>,
    /// The column number in the source file with zero based indexing.
    pub column: Option<ColumnType>,
    /// The file name of the source file.
    pub file: Option<String>,
    /// The directory of the source file.
    #[serde(serialize_with = "serialize_typed_path")]
    pub directory: Option<TypedPathBuf>,
}

impl Debug for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "line: {:?}, column: {:?}, in file: {:?}",
            self.line,
            self.column,
            match self.combined_typed_path().as_ref() {
                Some(path) => path.to_string_lossy(),
                None => std::borrow::Cow::Borrowed("<unspecified>"),
            }
        )
    }
}

impl SourceLocation {
    /// Resolve debug information for a [`Instruction`] and create a [`SourceLocation`].
    fn from_instruction(
        debug_info: &DebugInfo,
        program_unit: &unit_info::UnitInfo,
        instruction: &Instruction,
    ) -> Option<SourceLocation> {
        debug_info
            .find_file_and_directory(&program_unit.unit, instruction.file_index)
            .map(|(file, directory)| SourceLocation {
                address: instruction.address,
                line: instruction.line.map(std::num::NonZeroU64::get),
                column: Some(instruction.column),
                file,
                directory,
            })
    }

    /// Get the full path of the source file
    pub fn combined_typed_path(&self) -> Option<TypedPathBuf> {
        let combined_path = self
            .directory
            .as_ref()
            .and_then(|dir| self.file.as_ref().map(|file| dir.join(file)));

        combined_path
    }

    /// Return the first valid breakpoint location of the statement that is greater than OR equal to `address`.
    /// e.g., if the `address` is the current program counter, then the return value will be the next valid halt address
    /// in the current sequence, where the address is part of the sequence, and will NOT be bypassed because of branching.
    pub(crate) fn breakpoint_at_address(
        debug_info: &DebugInfo,
        address: u64,
    ) -> Result<Self, DebugError> {
        let sequence = Sequence::from_address(debug_info, address)?;

        if let Some(verified_breakpoint) = sequence.haltpoint_for_address(address) {
            tracing::debug!(
                "Found valid breakpoint for address: {:#010x} : {verified_breakpoint:?}",
                &address
            );
            return Ok(verified_breakpoint);
        }
        // If we get here, we have not found a valid breakpoint location.
        let message =
            format!("Could not identify a valid breakpoint for address: {address:#010x}.");
        Err(DebugError::WarnAndContinue { message })
    }

    /// Identifying the breakpoint location for a specific location (path, line, colunmn) is a bit more complex,
    /// compared to the `for_address()` method, due to a few factors:
    /// - The correct program instructions, may be in any of the compilation units of the current program.
    /// - The debug information may not contain data for the "specific source" location requested:
    ///   - DWARFv5 standard, section 6.2, allows omissions based on certain conditions. In this case,
    ///    we need to find the closest "relevant" source location that has valid debug information.
    ///   - The requested location may not be a valid source location, e.g. when the
    ///    debug information has been optimized away. In this case we will return an appropriate error.
    /// #### The logic used to find the "most relevant" source location is as follows:
    /// - Filter  [`UnitInfo`] , by using [`LineProgramHeader`] to match units that include the requested path.
    /// - For each matching compilation unit, get the [`LineProgram`] and [`Vec<LineSequence>`].
    /// - Convert [`LineSequence`], to [`Sequence`] to infer statement block boundaries.
    /// - Return the first `Instruction` that contains the requested source location, being one of the following:
    ///   - This may be an exact match on file/line/column, or,
    ///   - Failing an exact match, a match on file/line only.
    ///   - Failing that, a match on file only, where the line number is the "next" available instruction,
    ///     on the next available line of the specified file.
    pub(crate) fn breakpoint_at_source(
        debug_info: &DebugInfo,
        path: &TypedPathBuf,
        line: u64,
        column: Option<u64>,
    ) -> Result<Self, DebugError> {
        // Keep track of the matching file index to avoid having to lookup and match the full path
        // for every row in the program line sequence.
        let line_sequences_for_path = line_sequences_for_path(debug_info, path);
        for (sequence, matching_file_index) in &line_sequences_for_path {
            if let Some(verified_breakpoint) =
                sequence.haltpoint_for_source_location(*matching_file_index, line, column)
            {
                return Ok(verified_breakpoint);
            }
        }
        // If we get here, we need a "best effort" approach to find the next line in the file with a valid haltpoint.
        if let Some(verified_breakpoint) =
            SourceLocation::breakpoint_at_next_line(debug_info, &line_sequences_for_path, line)
        {
            return Ok(verified_breakpoint);
        }

        // If we get here, we have not found a valid breakpoint location.
        Err(DebugError::Other(anyhow::anyhow!("No valid breakpoint information found for file: {}, line: {line:?}, column: {column:?}", path.to_path().display())))
    }

    fn breakpoint_at_next_line(
        debug_info: &DebugInfo,
        file_sequences: &[(Sequence, Option<u64>)],
        line: u64,
    ) -> Option<Self> {
        let mut candidate_haltpoints: Vec<&Instruction> = Vec::new();
        for file_sequence in file_sequences {
            let (sequence, file_index) = file_sequence;
            candidate_haltpoints.extend(sequence.blocks.iter().flat_map(|block| {
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
        if let Some(matching_breakpoint) = candidate_haltpoints
            .iter()
            .find(|instruction| instruction.line > NonZeroU64::new(line))
            .and_then(|instruction| {
                SourceLocation::breakpoint_at_address(debug_info, instruction.address).ok()
            })
        {
            tracing::warn!("Suggesting a closely matching breakpoint: {matching_breakpoint:?}");
            Some(matching_breakpoint)
        } else {
            None
        }
    }
}

pub(crate) fn serialize_typed_path<S>(
    path: &Option<TypedPathBuf>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match path {
        Some(path) => serializer.serialize_str(&path.to_string_lossy()),
        None => serializer.serialize_none(),
    }
}

/// Uses the [std::fs::canonicalize] function to canonicalize both paths before applying the [TypedPathBuf::starts_with]
/// to test if the source file path is equal, or a split compilation unit of the source file.
/// We use 'starts_with` because the DWARF unit paths often have split unit identifiers, e.g. `...main.rs/@/11rwb6kiscqun26d`.
/// If for some reason (e.g., the paths don't exist) the canonicalization fails, the original equality check is used.
/// We do this to maximize the chances of finding a match where the source file path can be given as
/// an absolute, relative, or partial path.
pub(crate) fn canonical_unit_path_eq(
    unit_path: &TypedPathBuf,
    source_file_path: &TypedPathBuf,
) -> bool {
    unit_path
        .normalize()
        .starts_with(source_file_path.normalize())
}

/// Return the line program sequences with matching path entries, from all matching compilation units.
pub(crate) fn line_sequences_for_path<'a>(
    debug_info: &'a DebugInfo,
    path: &TypedPathBuf,
) -> Vec<(Sequence<'a>, Option<u64>)> {
    let mut line_sequences_for_path = Vec::new();
    for program_unit in debug_info.unit_infos.as_slice() {
        let Some(ref line_program) = program_unit.unit.line_program else {
            // Not all compilation units need to have debug line information, so we skip those.
            continue;
        };

        let mut matching_file_index = None;
        if line_program
            .header()
            .file_names()
            .iter()
            .enumerate()
            .any(|(file_index, _)| {
                debug_info
                    .get_path(&program_unit.unit, file_index as u64 + 1)
                    .map(|unit_path: TypedPathBuf| {
                        if canonical_unit_path_eq(&unit_path, path) {
                            // we use file_index + 1, because the file index is 1-based in DWARF.
                            matching_file_index = Some(file_index as u64 + 1);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false)
            })
        {
            let Ok((complete_line_program, line_sequences)) = line_program.clone().sequences()
            else {
                continue;
            };
            // Return all the sequences for the matching file index.
            for line_sequence in &line_sequences {
                if let Ok(sequence) = Sequence::from_line_sequence(
                    debug_info,
                    program_unit,
                    complete_line_program.clone(),
                    line_sequence,
                ) {
                    line_sequences_for_path.push((sequence, matching_file_index))
                };
            }
        }
    }
    line_sequences_for_path
}

/// Return the line program sequence which contain the instruction for the given address .
pub(crate) fn line_sequence_for_address(
    debug_info: &DebugInfo,
    address_filter: u64,
) -> Option<Sequence> {
    for program_unit in debug_info.unit_infos.as_slice() {
        let Some(ref line_program) = program_unit.unit.line_program else {
            continue;
        };
        let Ok((complete_line_program, line_sequences)) = line_program.clone().sequences() else {
            continue;
        };
        if let Some(sequence) = line_sequences
            .iter()
            .find(|sequence| sequence.start <= address_filter && sequence.end > address_filter)
            .and_then(|sequence| {
                Sequence::from_line_sequence(
                    debug_info,
                    program_unit,
                    complete_line_program.clone(),
                    sequence,
                )
                .ok()
            })
        {
            return Some(sequence);
        }
    }
    None
}
