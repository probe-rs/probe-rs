mod block;
mod breakpoint;
mod instruction;
mod sequence;
mod stepping;
use self::sequence::Sequence;
use super::{
    unit_info::{self},
    ColumnType, DebugInfo,
};
pub use breakpoint::VerifiedBreakpoint;
use core::fmt::Debug;
use instruction::Instruction;
pub use stepping::Stepping;
use typed_path::TypedPathBuf;

/// A specific location in source code, represented by an instruction address and source file information.
/// Not all instructions have a known (from debug info) source location.
/// Each unique line, column, file and directory combination is a unique source location.
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
