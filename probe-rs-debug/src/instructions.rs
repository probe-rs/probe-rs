mod block;
mod breakpoint;
mod instruction;
mod sequence;
use instruction::Instruction;

use super::{
    unit_info::{self},
    ColumnType, DebugInfo,
};
pub use breakpoint::VerifiedBreakpoint;
use serde::Serialize;
use typed_path::TypedPathBuf;

fn serialize_typed_path<S>(path: &TypedPathBuf, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serializer.serialize_str(&path.to_string_lossy())
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
