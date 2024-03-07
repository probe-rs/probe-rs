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
use serde::Serialize;
use typed_path::{TypedPath, TypedPathBuf};

fn serialize_typed_path<S>(path: &TypedPathBuf, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
}

/// Uses the [std::fs::canonicalize] function to canonicalize both paths before applying the [TypedPathBuf::starts_with]
/// to test if the source file path is equal, or a split compilation unit of the source file.
/// We use 'starts_with` because the DWARF unit paths often have split unit identifiers, e.g. `...main.rs/@/11rwb6kiscqun26d`.
/// If for some reason (e.g., the paths don't exist) the canonicalization fails, the original equality check is used.
/// We do this to maximize the chances of finding a match where the source file path can be given as
/// an absolute, relative, or partial path.
pub(crate) fn canonical_unit_path_eq(unit_path: TypedPath, source_file_path: TypedPath) -> bool {
    unit_path
        .normalize()
        .starts_with(source_file_path.normalize())
}

/// A specific location in source code.
/// Each unique line, column, file and directory combination is a unique source location.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct SourceLocation {
    /// The path to the source file
    #[serde(serialize_with = "serialize_typed_path")]
    pub path: TypedPathBuf,
    /// The line number in the source file with zero based indexing.
    pub line: Option<u64>,
    /// The column number in the source file.
    pub column: Option<ColumnType>,
    /// The address of the source location.
    pub address: Option<u64>,
}

impl std::fmt::Debug for SourceLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{:?}:{:?}",
            self.path.to_path().display(),
            self.line,
            self.column
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
            .map(|path| SourceLocation {
                line: instruction.line.map(std::num::NonZeroU64::get),
                column: Some(instruction.column),
                path,
                address: Some(instruction.address),
            })
    }

    /// Get the file name of the source file
    pub fn file_name(&self) -> Option<String> {
        self.path
            .file_name()
            .map(|name| String::from_utf8_lossy(name).to_string())
    }
}
