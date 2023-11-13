use super::*;
use crate::core::RegisterValue;
use std;

/// A full stack frame with all its information contained.
#[derive(Debug, Default, PartialEq)]
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
            let separator = match &si.directory {
                Some(path) if path.is_windows() => '\\',
                _ => '/',
            };

            write!(
                f,
                "\t{}{}{}",
                si.directory
                    .as_ref()
                    .map(|p| p.to_string_lossy())
                    .unwrap_or_else(|| std::borrow::Cow::from("<unknown dir>")),
                separator,
                si.file.as_ref().unwrap_or(&"<unknown file>".to_owned())
            )?;

            if let (Some(column), Some(line)) = (si.column, si.line) {
                match column {
                    ColumnType::Column(c) => write!(f, ":{line}:{c}")?,
                    ColumnType::LeftEdge => write!(f, ":{line}")?,
                }
            }
        }
        writeln!(f)
    }
}

#[cfg(test)]
pub struct TestFormatter<'s>(pub &'s StackFrame);

#[cfg(test)]
impl<'s> std::fmt::Display for TestFormatter<'s> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(f, "Frame:")?;
        writeln!(f, " function:        {}", self.0.function_name)?;

        writeln!(f, " source_location: ")?;
        match &self.0.source_location {
            Some(location) => {
                write!(f, "  directory: ")?;
                match location.directory.as_ref() {
                    Some(l) => writeln!(f, "{}", l.to_path().display())?,
                    None => writeln!(f, "None")?,
                }
                writeln!(f, "  file: {:?}", location.file)?;
                writeln!(f, "  line: {:?}", location.line)?;
                writeln!(f, "  column: {:?}", location.column)?;
            }
            None => writeln!(f, "None")?,
        }
        writeln!(f, " frame_base:      {:08x?}", self.0.frame_base)?;

        Ok(())
    }
}
