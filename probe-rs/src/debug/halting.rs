mod block;
mod breakpoint;
mod instruction;
mod sequence;
use super::{
    unit_info::{self},
    ColumnType, DebugInfo,
};
pub use breakpoint::VerifiedBreakpoint;
use instruction::Instruction;
use typed_path::TypedPathBuf;

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

/// A specific location in source code.
/// Each unique line, column, file and directory combination is a unique source location.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
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
