#![warn(missing_docs)]

//! Debugging support for probe-rs
//!
//! The `debug` module contains various debug functionality, which can be
//! used to implement a debugger based on `probe-rs`.

/// Debug information which is parsed from DWARF debugging information.
pub mod debug_info;
/// References to the DIE (debug information entry) of functions.
pub mod function_die;
/// Identifying source locations and instruction addresses for debug unwind, stepping and breakpoints.
pub(crate) mod halting;
/// Programming languages
pub(crate) mod language;
/// Target Register definitions, expanded from [`crate::core::registers::CoreRegister`] to include unwind specific information.
pub mod registers;
/// The stack frame information used while unwinding the stack from a specific program counter.
pub mod stack_frame;
/// Information about a Unit in the debug information.
pub mod unit_info;
/// Variable information used during debug.
pub mod variable;
/// The hierarchical cache of all variables for a given scope.
pub mod variable_cache;

pub use self::{
    debug_info::*,
    halting::{SourceLocation, Stepping, VerifiedBreakpoint},
    registers::*,
    stack_frame::StackFrame,
    variable::*,
    variable_cache::VariableCache,
};
use crate::{core::Core, MemoryInterface};

use gimli::DebuggingInformationEntry;
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
    /// Error while identifying a valid halt location for setting a breakpoint, or during debug stepping.
    #[error("{0}. Please consider using instruction level stepping, or try setting a breakpoint at a different location.")]
    HaltLocation(&'static str),
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
        if value < 0 {
            ObjectRef::Invalid
        } else {
            match NonZeroU32::try_from(value as u32) {
                Ok(v) => ObjectRef::Valid(v),
                Err(_) => ObjectRef::Invalid,
            }
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
    attribute_value: gimli::AttributeValue<GimliReader>,
) -> Option<(TypedPathBuf, String)> {
    match attribute_value {
        gimli::AttributeValue::FileIndex(index) => {
            if let Some((Some(path), Some(file))) = debug_info
                .find_file_and_directory(unit, index)
                .map(|(file, path)| (path, file))
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
        Ok(optional_byte_size_attr) => match optional_byte_size_attr {
            Some(byte_size_attr) => match byte_size_attr.value() {
                gimli::AttributeValue::Udata(byte_size) => Some(byte_size),
                gimli::AttributeValue::Data1(byte_size) => Some(byte_size as u64),
                gimli::AttributeValue::Data2(byte_size) => Some(byte_size as u64),
                gimli::AttributeValue::Data4(byte_size) => Some(byte_size as u64),
                gimli::AttributeValue::Data8(byte_size) => Some(byte_size),
                other => {
                    tracing::warn!("Unimplemented: DW_AT_byte_size value: {:?} ", other);
                    None
                }
            },
            None => None,
        },
        Err(error) => {
            tracing::warn!(
                "Failed to extract byte_size: {:?} for debug_entry {:?}",
                error,
                node_die.tag().static_string()
            );
            None
        }
    }
}

fn extract_line(attribute_value: gimli::AttributeValue<GimliReader>) -> Option<u64> {
    match attribute_value {
        gimli::AttributeValue::Udata(line) => Some(line),
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
        for _ in 0..(print_depth) {
            print!("\t");
        }
        print!("{}: ", attr.name());

        use gimli::AttributeValue::*;

        match attr.value() {
            Addr(a) => println!("{a:#010x}"),
            DebugStrRef(_) => {
                let val = dwarf.attr_string(unit, attr.value()).unwrap();
                println!("{}", std::str::from_utf8(&val).unwrap());
            }
            Exprloc(e) => {
                let mut evaluation = e.evaluation(unit.encoding());

                // go for evaluation
                let mut result = evaluation.evaluate().unwrap();

                loop {
                    use gimli::EvaluationResult::*;

                    result = match result {
                        Complete => break,
                        RequiresMemory { address, size, .. } => {
                            let mut buff = vec![0u8; size as usize];
                            core.read(address, &mut buff)
                                .expect("Failed to read memory");
                            match size {
                                1 => evaluation
                                    .resume_with_memory(gimli::Value::U8(buff[0]))
                                    .unwrap(),
                                2 => {
                                    let val = u16::from(buff[0]) << 8 | u16::from(buff[1]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U16(val))
                                        .unwrap()
                                }
                                4 => {
                                    let val = u32::from(buff[0]) << 24
                                        | u32::from(buff[1]) << 16
                                        | u32::from(buff[2]) << 8
                                        | u32::from(buff[3]);
                                    evaluation
                                        .resume_with_memory(gimli::Value::U32(val))
                                        .unwrap()
                                }
                                x => {
                                    tracing::error!(
                                        "Requested memory with size {}, which is not supported yet.",
                                        x
                                    );
                                    unimplemented!();
                                }
                            }
                        }
                        RequiresFrameBase => evaluation
                            .resume_with_frame_base(stackframe_cfa.unwrap())
                            .unwrap(),
                        RequiresRegister {
                            register,
                            base_type,
                        } => {
                            let raw_value: u64 = core
                                .read_core_reg(register.0)
                                .expect("Failed to read memory");

                            if base_type != gimli::UnitOffset(0) {
                                unimplemented!(
                                    "Support for units in RequiresRegister request is not yet implemented."
                                )
                            }
                            evaluation
                                .resume_with_register(gimli::Value::Generic(raw_value))
                                .unwrap()
                        }
                        RequiresRelocatedAddress(address_index) => {
                            // Use the address_index as an offset from 0, so just pass it into the next step.
                            evaluation
                                .resume_with_relocated_address(address_index)
                                .unwrap()
                        }
                        x => {
                            println!("print_all_attributes {x:?}");
                            // x
                            todo!()
                        }
                    }
                }

                let result = evaluation.result();

                println!("Expression: {:x?}", &result[0]);
            }
            LocationListsRef(_) => {
                println!("LocationList");
            }
            DebugLocListsBase(_) => {
                println!(" LocationList");
            }
            DebugLocListsIndex(_) => {
                println!(" LocationList");
            }
            _ => {
                println!("print_all_attributes {:?}", attr.value());
            }
        }
    }
}
