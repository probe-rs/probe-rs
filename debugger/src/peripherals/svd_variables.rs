use std::fmt::Debug;

use crate::DebuggerError;
use probe_rs::debug::VariableCache;
use svd_rs::{peripheral, Device, MaybeArray::Single, PeripheralInfo};

/// The SVD file contents and related data
#[derive(Debug)]
pub(crate) struct SvdCache {
    /// A unique identifier
    pub(crate) id: i64,
    /// The Device file represents the top element in a SVD file.
    pub(crate) svd_device: Device,
    /// The SVD contents and structure will be stored as variables, down to the Register level.
    /// Unlike other VariableCache instances, it will only be built once per DebugSession.
    /// After that, only the SVD fields change values, and the data for these will be re-read everytime they are queried by the debugger.
    pub(crate) svd_registers: VariableCache,
}
