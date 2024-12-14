use super::*;
use crate::core::RegisterValue;

#[cfg(test)]
pub use test::TestFormatter;

/// Helper struct to pass around multiple pieces of `StackFrame` related information.
#[derive(Clone, Copy)]
pub struct StackFrameInfo<'a> {
    /// The current register state represented in this stackframe.
    pub registers: &'a registers::DebugRegisters,

    /// The DWARF debug info defines a `DW_AT_frame_base` attribute which can be used to calculate the memory location of variables in a stack frame.
    /// The rustc compiler, has a compile flag, `-C force-frame-pointers`, which when set to `on`, will usually result in this being a pointer to the register value of the platform frame pointer.
    /// However, some isa's (e.g. RISC-V) uses a default of `-C force-frame-pointers off` and will then use the stack pointer as the frame base address.
    /// We store the frame_base of the relevant non-inlined parent function, to ensure correct calculation of the [`Variable::memory_location`] values.
    pub frame_base: Option<u64>,

    /// The value of the stack pointer just before the CALL instruction in the parent function.
    pub canonical_frame_address: Option<u64>,
}

/// A full stack frame with all its information contained.
#[derive(PartialEq, Serialize)]
pub struct StackFrame {
    /// The stackframe ID.
    #[serde(skip_serializing)]
    pub id: ObjectRef,
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
    /// However, some isa's (e.g. RISC-V) uses a default of `-C force-frame-pointers off` and will then use the stack pointer as the frame base address.
    /// We store the frame_base of the relevant non-inlined parent function, to ensure correct calculation of the [`Variable::memory_location`] values.
    pub frame_base: Option<u64>,
    /// Indicate if this stack frame belongs to an inlined function.
    pub is_inlined: bool,
    /// A cache of 'local' scoped variables for this stackframe, with a `Variable` for each in-scope variable.
    /// - Complex variables and pointers will have additional children.
    ///   - This structure is recursive until a base type is encountered.
    pub local_variables: Option<VariableCache>,
    /// The value of the stack pointer just before the CALL instruction in the parent function.
    pub canonical_frame_address: Option<u64>,
}

impl std::fmt::Display for StackFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        // Header info for the StackFrame
        writeln!(f, "Frame: {}", self.function_name)?;
        if let Some(si) = &self.source_location {
            write!(f, "\t{}", si.path.to_path().display())?;

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
mod test {
    use super::StackFrame;

    /// Helper struct used to format a StackFrame for testing.
    pub struct TestFormatter<'s>(pub &'s StackFrame);

    impl std::fmt::Display for TestFormatter<'_> {
        fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            writeln!(f, "Frame:")?;
            writeln!(f, " function:        {}", self.0.function_name)?;

            writeln!(f, " source_location:")?;
            match &self.0.source_location {
                Some(location) => {
                    writeln!(f, "  path: {}", location.path.to_path().display())?;
                    writeln!(f, "  line: {:?}", location.line)?;
                    writeln!(f, "  column: {:?}", location.column)?;
                }
                None => writeln!(f, "None")?,
            }
            writeln!(f, " frame_base:      {:08x?}", self.0.frame_base)?;

            Ok(())
        }
    }
}
