use crate::RegisterValue;

use super::*;
use std;

/// A full stack frame with all its information contained.
#[derive(Debug)]
pub struct StackFrame {
    /// The stackframe ID.
    pub id: i64,
    /// The name of the function this stackframe belongs to.
    pub function_name: String,
    /// The source location the function this stackframe belongs to originates.
    pub source_location: Option<SourceLocation>,
    /// The current register state represented in this stackframe.
    pub registers: registers::DebugRegisters,
    /// The program counter / address of the current instruction when this stack frame was created
    pub pc: RegisterValue,
    /// The DWARF debug info defines a `DW_AT_frame_base` attribute which can be used to calculate the memory location of variables in a stack frame.
    /// The rustc compiler, has a compile flag, `-C force-frame-pointers`, which when set to `on`, will usually result in this being a pointer to the register value of the platform frame pointer.
    /// However, some isa's (e.g. RISCV) uses a default of `-C force-frame-pointers off` and will then use the stack pointer as the frame base address.
    /// We store the frame_base of the relevant non-inlined parent function, to ensure correct calculation of the [`Variable::memory_location`] values.
    pub frame_base: Option<u64>,
    /// Indicate if this stack frame belongs to an inlined function.
    pub is_inlined: bool,
    /// A cache of 'static' scoped variables for this stackframe
    pub static_variables: Option<VariableCache>,
    /// A cache of 'local' scoped variables for this stafckframe, with a `Variable` for each in-scope variable.
    /// - Complex variables and pointers will have additional children.
    ///   - This structure is recursive until a base type is encountered.
    pub local_variables: Option<VariableCache>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // Header info for the StackFrame
        writeln!(f, "Frame: {}", self.function_name)?;
        if let Some(si) = &self.source_location {
            write!(
                f,
                "\t{}/{}",
                si.directory
                    .as_ref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_else(|| std::borrow::Cow::from("<unknown dir>")),
                si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
            )?;

            if let (Some(column), Some(line)) = (si.column, si.line) {
                match column {
                    ColumnType::Column(c) => write!(f, ":{}:{}", line, c)?,
                    ColumnType::LeftEdge => write!(f, ":{}", line)?,
                }
            }
        }
        writeln!(f)
    }
}
