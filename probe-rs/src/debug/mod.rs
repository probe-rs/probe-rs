//! Debugging support for probe-rs
//!
//! The `debug` module contains various debug functionality, which can be
//! used to implement a debugger based on `probe-rs`.

/// Debug information which is parsed from DWARF debugging information.
pub mod debug_info;
/// Stepping through a program during debug, at various granularities.
pub mod debug_step;
mod exception_handling;
/// References to the DIE (debug information entry) of functions.
pub mod function_die;
/// Programming languages
pub(crate) mod language;
/// Target Register definitions, expanded from [`crate::core::registers::CoreRegister`] to include unwind specific information.
pub mod registers;
/// The source statement information used while identifying haltpoints for debug stepping and breakpoints.
pub(crate) mod source_instructions;
/// The stack frame information used while unwinding the stack from a specific program counter.
pub mod stack_frame;
/// Information about a Unit in the debug information.
pub mod unit_info;
/// Variable information used during debug.
pub mod variable;
/// The hierarchical cache of all variables for a given scope.
pub mod variable_cache;

pub use self::{
    debug_info::*, debug_step::SteppingMode, registers::*, source_instructions::SourceLocation,
    source_instructions::VerifiedBreakpoint, stack_frame::StackFrame, variable::*,
    variable_cache::VariableCache,
};
use crate::Error;
use crate::{core::Core, MemoryInterface};

use gimli::AttributeValue;
use gimli::DebuggingInformationEntry;
use gimli::EvaluationResult;
use probe_rs_target::CoreType;
use serde::Serialize;
use typed_path::TypedPathBuf;

use std::{
    io,
    num::NonZeroU32,
    str::Utf8Error,
    sync::atomic::{AtomicU32, Ordering},
    vec,
};

/// A simplified type alias of the [`gimli::EndianReader`] type.
pub type EndianReader = gimli::EndianReader<gimli::LittleEndian, std::rc::Rc<[u8]>>;

/// An error occurred while debugging the target.
#[derive(Debug, thiserror::Error)]
pub enum DebugError {
    /// An IO error occurred when accessing debug data.
    #[error("IO Error while accessing debug data")]
    Io(#[from] io::Error),
    /// An error occurred while accessing debug data.
    #[error("Error accessing debug data")]
    DebugData(#[from] object::read::Error),
    /// Something failed while parsing debug data.
    #[error("Error parsing debug data")]
    Parse(#[from] gimli::read::Error),
    /// Non-UTF8 data was found in the debug data.
    #[error("Non-UTF8 data found in debug data")]
    NonUtf8(#[from] Utf8Error),
    /// A probe-rs error occurred.
    #[error("Error using the probe")]
    Probe(#[from] crate::Error),
    /// A char could not be created from the given string.
    #[error(transparent)]
    CharConversion(#[from] std::char::CharTryFromError),
    /// An int could not be created from the given string.
    #[error(transparent)]
    IntConversion(#[from] std::num::TryFromIntError),
    /// Non-terminal Errors encountered while unwinding the stack, e.g. Could not resolve the value of a variable in the stack.
    /// These are distinct from other errors because they do not interrupt processing.
    /// Instead, the cause of incomplete results are reported back/explained to the user, and the stack continues to unwind.
    #[error("{message}")]
    WarnAndContinue {
        /// A message that can be displayed to the user to help them understand the reason for the incomplete results.
        message: String,
    },
    /// Some other error occurred.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// A copy of [`gimli::ColumnType`] which uses [`u64`] instead of [`NonZeroU64`](std::num::NonZeroU64).
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize)]
pub enum ColumnType {
    /// The `LeftEdge` means that the statement begins at the start of the new line.
    LeftEdge,
    /// A column number, whose range begins at 1.
    Column(u64),
}

impl From<gimli::ColumnType> for ColumnType {
    fn from(column: gimli::ColumnType) -> Self {
        match column {
            gimli::ColumnType::LeftEdge => ColumnType::LeftEdge,
            gimli::ColumnType::Column(c) => ColumnType::Column(c.get()),
        }
    }
}

impl From<u64> for ColumnType {
    fn from(column: u64) -> Self {
        match column {
            0 => ColumnType::LeftEdge,
            _ => ColumnType::Column(column),
        }
    }
}

/// Object reference as defined in the DAP standard.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ObjectRef {
    /// Valid object reference (> 0)
    Valid(NonZeroU32),
    /// Invalid object reference (<= 0)
    #[default]
    Invalid,
}

impl PartialOrd for ObjectRef {
    fn partial_cmp(&self, other: &ObjectRef) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ObjectRef {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        i64::from(*self).cmp(&i64::from(*other))
    }
}

impl From<ObjectRef> for i64 {
    fn from(value: ObjectRef) -> Self {
        match value {
            ObjectRef::Valid(v) => v.get() as i64,
            ObjectRef::Invalid => 0,
        }
    }
}

impl From<i64> for ObjectRef {
    fn from(value: i64) -> Self {
        if value > 0 {
            ObjectRef::Valid(NonZeroU32::try_from(value as u32).unwrap())
        } else {
            ObjectRef::Invalid
        }
    }
}

impl std::str::FromStr for ObjectRef {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let value = s.parse::<i64>()?;
        Ok(ObjectRef::from(value))
    }
}

static CACHE_KEY: AtomicU32 = AtomicU32::new(1);
/// Generate a unique key that can be used to assign id's to StackFrame and Variable structs.
pub fn get_object_reference() -> ObjectRef {
    let key = CACHE_KEY.fetch_add(1, Ordering::SeqCst);
    ObjectRef::Valid(NonZeroU32::new(key).unwrap())
}

/// If file information is available, it returns `Some(directory:PathBuf, file_name:String)`, otherwise `None`.
fn extract_file(
    debug_info: &DebugInfo,
    unit: &gimli::Unit<GimliReader>,
    attribute_value: AttributeValue<GimliReader>,
) -> Option<(TypedPathBuf, String)> {
    match attribute_value {
        AttributeValue::FileIndex(index) => {
            if let Some((Some(file), Some(path))) = debug_info.find_file_and_directory(unit, index)
            {
                Some((path, file))
            } else {
                tracing::warn!("Unable to extract file or path from {:?}.", attribute_value);
                None
            }
        }
        other => {
            tracing::warn!(
                "Unable to extract file information from attribute value {:?}: Not implemented.",
                other
            );
            None
        }
    }
}

/// If a DW_AT_byte_size attribute exists, return the u64 value, otherwise (including errors) return None
fn extract_byte_size(node_die: &DebuggingInformationEntry<GimliReader>) -> Option<u64> {
    match node_die.attr(gimli::DW_AT_byte_size) {
        Ok(Some(byte_size_attr)) => match byte_size_attr.value() {
            AttributeValue::Udata(byte_size) => Some(byte_size),
            AttributeValue::Data1(byte_size) => Some(byte_size as u64),
            AttributeValue::Data2(byte_size) => Some(byte_size as u64),
            AttributeValue::Data4(byte_size) => Some(byte_size as u64),
            AttributeValue::Data8(byte_size) => Some(byte_size),
            other => {
                tracing::warn!("Unimplemented: DW_AT_byte_size value: {other:?}");
                None
            }
        },
        Ok(None) => None,
        Err(error) => {
            tracing::warn!(
                "Failed to extract byte_size: {error:?} for debug_entry {:?}",
                node_die.tag().static_string()
            );
            None
        }
    }
}

fn extract_line(attribute_value: AttributeValue<GimliReader>) -> Option<u64> {
    match attribute_value {
        AttributeValue::Udata(line) => Some(line),
        _ => None,
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) fn _print_all_attributes(
    core: &mut Core<'_>,
    stackframe_cfa: Option<u64>,
    dwarf: &gimli::Dwarf<DwarfReader>,
    unit: &gimli::Unit<DwarfReader>,
    tag: &gimli::DebuggingInformationEntry<DwarfReader>,
    print_depth: usize,
) {
    let mut attrs = tag.attrs();

    while let Some(attr) = attrs.next().unwrap() {
        for _ in 0..print_depth {
            print!("\t");
        }
        print!("{}: ", attr.name());

        match attr.value() {
            AttributeValue::Addr(a) => println!("{a:#010x}"),
            AttributeValue::DebugStrRef(str_ref) => {
                let val = dwarf.string(str_ref).unwrap();
                println!("{}", std::str::from_utf8(&val).unwrap());
            }
            AttributeValue::Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                while let Some(next) = iterate(result, core, &mut evaluation, stackframe_cfa) {
                    result = next;
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            }
            AttributeValue::LocationListsRef(_) => println!("LocationList"),
            AttributeValue::DebugLocListsBase(_) => println!(" LocationList"),
            AttributeValue::DebugLocListsIndex(_) => println!(" LocationList"),
            _ => println!("print_all_attributes {:?}", attr.value()),
        }
    }
}

#[allow(dead_code)]
fn iterate(
    result: EvaluationResult<DwarfReader>,
    core: &mut Core,
    evaluation: &mut gimli::Evaluation<DwarfReader>,
    stackframe_cfa: Option<u64>,
) -> Option<EvaluationResult<DwarfReader>> {
    let resume_result = match result {
        EvaluationResult::Complete => return None,
        EvaluationResult::RequiresMemory { address, size, .. } => {
            let mut buff = vec![0u8; size as usize];
            core.read(address, &mut buff)
                .expect("Failed to read memory");

            let value = match size {
                1 => gimli::Value::U8(buff[0]),
                2 => gimli::Value::U16(u16::from_be_bytes([buff[0], buff[1]])),
                4 => gimli::Value::U32(u32::from_be_bytes([buff[0], buff[1], buff[2], buff[3]])),
                x => unimplemented!("Requested memory with size {x}, which is not supported yet."),
            };

            evaluation.resume_with_memory(value)
        }
        EvaluationResult::RequiresFrameBase => {
            evaluation.resume_with_frame_base(stackframe_cfa.unwrap())
        }
        EvaluationResult::RequiresRegister {
            register,
            base_type,
        } => {
            let raw_value = core
                .read_core_reg::<u64>(register.0)
                .expect("Failed to read memory");

            if base_type != gimli::UnitOffset(0) {
                unimplemented!(
                    "Support for units in RequiresRegister request is not yet implemented."
                )
            }
            evaluation.resume_with_register(gimli::Value::Generic(raw_value))
        }
        EvaluationResult::RequiresRelocatedAddress(address_index) => {
            // Use the address_index as an offset from 0, so just pass it into the next step.
            evaluation.resume_with_relocated_address(address_index)
        }
        x => {
            println!("print_all_attributes {x:?}");
            // x
            todo!()
        }
    };

    Some(resume_result.unwrap())
}

/// Creates a new exception interface for the [`CoreType`] at hand.
pub fn exception_handler_for_core(core_type: CoreType) -> Box<dyn ExceptionInterface> {
    use exception_handling::{armv6m, armv7m, armv8m};
    match core_type {
        CoreType::Armv6m => Box::new(armv6m::ArmV6MExceptionHandler),
        CoreType::Armv7m | CoreType::Armv7em => Box::new(armv7m::ArmV7MExceptionHandler),
        CoreType::Armv8m => Box::new(armv8m::ArmV8MExceptionHandler),
        CoreType::Armv7a | CoreType::Armv8a | CoreType::Riscv | CoreType::Xtensa => {
            Box::new(UnimplementedExceptionHandler)
        }
    }
}

/// Placeholder for exception handling for cores where handling exceptions is not yet supported.
pub struct UnimplementedExceptionHandler;

impl ExceptionInterface for UnimplementedExceptionHandler {
    fn exception_details(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &DebugRegisters,
        _debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, Error> {
        // For architectures where the exception handling has not been implemented in probe-rs,
        // this will result in maintaining the current `unwind` behavior, i.e. unwinding will include up
        // to the first frame that was called from an exception handler.
        Ok(None)
    }

    fn calling_frame_registers(
        &self,
        _memory: &mut dyn MemoryInterface,
        _stackframe_registers: &crate::debug::DebugRegisters,
        _raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, crate::Error> {
        Err(Error::NotImplemented("calling frame registers"))
    }

    fn raw_exception(
        &self,
        _stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, crate::Error> {
        Err(Error::NotImplemented(
            "Not implemented for this architecture.",
        ))
    }

    fn exception_description(
        &self,
        _raw_exception: u32,
        _memory: &mut dyn MemoryInterface,
    ) -> Result<String, crate::Error> {
        Err(Error::NotImplemented("exception description"))
    }
}

/// A struct containing key information about an exception.
/// The exception details are architecture specific, and the abstraction is handled in the
/// architecture specific implementations of [`crate::core::ExceptionInterface`].
#[derive(PartialEq)]
pub struct ExceptionInfo {
    /// The exception number.
    /// This is architecture specific and can be used to decode the architecture specific exception reason.
    pub raw_exception: u32,
    /// A human readable explanation for the exception.
    pub description: String,
    /// A populated [`StackFrame`] to represent the stack data in the exception handler.
    pub handler_frame: StackFrame,
}

/// A generic interface to identify and decode exceptions during unwind processing.
#[cfg(feature = "debug")]
pub trait ExceptionInterface {
    /// Using the `stackframe_registers` for a "called frame",
    /// determine if the given frame was called from an exception handler,
    /// and resolve the relevant details about the exception, including the reason for the exception,
    /// and the stackframe registers for the frame that triggered the exception.
    /// A return value of `Ok(None)` indicates that the given frame was called from within the current thread,
    /// and the unwind should continue normally.
    fn exception_details(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &DebugRegisters,
        debug_info: &DebugInfo,
    ) -> Result<Option<ExceptionInfo>, Error>;

    /// Using the `stackframe_registers` for a "called frame", retrieve updated register values for the "calling frame".
    fn calling_frame_registers(
        &self,
        memory: &mut dyn MemoryInterface,
        stackframe_registers: &crate::debug::DebugRegisters,
        raw_exception: u32,
    ) -> Result<crate::debug::DebugRegisters, crate::Error>;

    /// Retrieve the architecture specific exception number.
    fn raw_exception(
        &self,
        stackframe_registers: &crate::debug::DebugRegisters,
    ) -> Result<u32, crate::Error>;

    /// Convert the architecture specific exception number into a human readable description.
    /// Where possible, the implementation may read additional registers from the core, to provide additional context.
    fn exception_description(
        &self,
        raw_exception: u32,
        memory: &mut dyn MemoryInterface,
    ) -> Result<String, crate::Error>;
}
