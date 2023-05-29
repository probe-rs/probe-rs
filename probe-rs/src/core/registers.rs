//! Core registers are represented by the `CoreRegister` struct, and collected in a `RegisterFile` for each of the supported architectures.

use crate::Error;
use anyhow::{anyhow, Result};
use std::{
    cmp::Ordering,
    convert::Infallible,
    fmt::{Display, Formatter},
};

/// The type of data stored in a register
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterDataType {
    /// Unsigned integer data
    UnsignedInteger,
    /// Floating point data
    FloatingPoint,
}

/// This is used to label the register with a specific role that it plays during program execution and exception handling.
/// The intention here is to harmonize the actual purpose of a register (e.g. `return address`),
/// while the [`CoreRegister::name`] will contain the architecture specific label of the register.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RegisterRole {
    /// Argument/Result registers like "A0", "a1", "r2", etc. (uses architecture specific names)
    Argument(&'static str),
    ProgramCounter,
    FramePointer,
    StackPointer,
    MainStackPointer,
    ProcessStackPointer,
    ProcessorStatus,
    ReturnAddress,
    FloatingPoint,
    FloatingPointStatus,
    Other(&'static str),
}

impl Display for RegisterRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterRole::Argument(name) => write!(f, "{}", name),
            RegisterRole::ProgramCounter => write!(f, "PC"),
            RegisterRole::FramePointer => write!(f, "FP"),
            RegisterRole::StackPointer => write!(f, "SP"),
            RegisterRole::MainStackPointer => write!(f, "MSP"),
            RegisterRole::ProcessStackPointer => write!(f, "PSP"),
            RegisterRole::ProcessorStatus => write!(f, "PSR"),
            RegisterRole::ReturnAddress => write!(f, "LR"),
            RegisterRole::FloatingPoint => write!(f, "FPU"),
            RegisterRole::FloatingPointStatus => write!(f, "FPSR"),
            RegisterRole::Other(name) => write!(f, "{}", name),
        }
    }
}

/// Describes a core (or CPU / hardware) register with its properties.
/// Each architecture will have a set of general purpose registers, and potentially some special purpose registers. It also happens that some general purpose registers can be used for special purposes. For instance, some ARM variants allows the `LR` (link register / return address) to be used as general purpose register `R14`."
#[derive(Debug, Clone, PartialEq)]
pub struct CoreRegister {
    /// The architecture specific name of the register. This may be identical, or similar to [`RegisterRole`].
    pub(crate) name: &'static str,
    pub(crate) id: RegisterId,
    /// If the register plays a special role during program execution and exception handling, this field will be set to the role.
    /// Otherwise, it will be `None` and the register is a general purpose register.
    pub(crate) role: Option<RegisterRole>,
    pub(crate) data_type: RegisterDataType,
    pub(crate) size_in_bits: usize,
}

impl Display for CoreRegister {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self.role {
            // These register roles have their own names assigned in their definition.
            Some(RegisterRole::Argument(name)) => write!(f, "{}", name),
            Some(RegisterRole::Other(name)) => write!(f, "{}", name),
            Some(RegisterRole::FloatingPoint) => write!(f, "{}", self.name),
            // The remainder roles get their name from the [`RegisterRole::Display`] implementation.
            Some(other_role) => write!(f, "{}", other_role),
            // If there is no role, use the name of the register.
            None => write!(f, "{}", self.name),
        }
    }
}

impl CoreRegister {
    /// Get the display name of this register
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Get the id of this register
    pub fn id(&self) -> RegisterId {
        self.id
    }

    /// Get the type of data stored in this register
    pub fn data_type(&self) -> RegisterDataType {
        self.data_type.clone()
    }

    /// Get the size, in bits, of this register
    pub fn size_in_bits(&self) -> usize {
        self.size_in_bits
    }

    /// Get the size, in bytes, of this register
    pub fn size_in_bytes(&self) -> usize {
        // Always round up
        (self.size_in_bits + 7) / 8
    }

    /// Get the width to format this register as a hex string
    /// Assumes a format string like `{:#0<width>x}`
    pub fn format_hex_width(&self) -> usize {
        (self.size_in_bytes() * 2) + 2
    }
}

impl From<CoreRegister> for RegisterId {
    fn from(description: CoreRegister) -> RegisterId {
        description.id
    }
}

impl From<&CoreRegister> for RegisterId {
    fn from(description: &CoreRegister) -> RegisterId {
        description.id
    }
}

/// The location of a CPU \register. This is not an actual memory address, but a core specific location that represents a specific core register.
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash)]
pub struct RegisterId(pub u16);

impl From<RegisterId> for u32 {
    fn from(value: RegisterId) -> Self {
        u32::from(value.0)
    }
}

impl From<u16> for RegisterId {
    fn from(value: u16) -> Self {
        RegisterId(value)
    }
}

/// A value of a core register
///
/// Creating a new `RegisterValue` should be done using From or Into.
/// Converting a value back to a primitive type can be done with either
/// a match arm or TryInto
#[derive(Debug, Clone, Copy)]
pub enum RegisterValue {
    /// 32-bit unsigned integer
    U32(u32),
    /// 64-bit unsigned integer
    U64(u64),
    /// 128-bit unsigned integer, often used with SIMD / FP
    U128(u128),
}

impl RegisterValue {
    /// A helper function to increment an address by a fixed number of bytes.
    pub fn increment_address(&mut self, bytes: usize) -> Result<(), Error> {
        match self {
            RegisterValue::U32(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u32) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(anyhow!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes,
                        self
                    )))
                }
            }
            RegisterValue::U64(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u64) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(anyhow!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes,
                        self
                    )))
                }
            }
            RegisterValue::U128(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u128) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(anyhow!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes,
                        self
                    )))
                }
            }
        }
    }

    /// A helper function to determine if the contained register value is equal to the maximum value that can be stored in that datatype.
    pub fn is_max_value(&self) -> bool {
        match self {
            RegisterValue::U32(register_value) => *register_value == u32::MAX,
            RegisterValue::U64(register_value) => *register_value == u64::MAX,
            RegisterValue::U128(register_value) => *register_value == u128::MAX,
        }
    }

    /// A helper function to determine if the contained register value is zero.
    pub fn is_zero(&self) -> bool {
        matches!(
            self,
            RegisterValue::U32(0) | RegisterValue::U64(0) | RegisterValue::U128(0)
        )
    }
}

impl Default for RegisterValue {
    fn default() -> Self {
        // Smallest data storage as default.
        RegisterValue::U32(0_u32)
    }
}

impl PartialOrd for RegisterValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let self_value = match self {
            RegisterValue::U32(self_value) => *self_value as u128,
            RegisterValue::U64(self_value) => *self_value as u128,
            RegisterValue::U128(self_value) => *self_value,
        };
        let other_value = match other {
            RegisterValue::U32(other_value) => *other_value as u128,
            RegisterValue::U64(other_value) => *other_value as u128,
            RegisterValue::U128(other_value) => *other_value,
        };
        self_value.partial_cmp(&other_value)
    }
}

impl PartialEq for RegisterValue {
    fn eq(&self, other: &Self) -> bool {
        let self_value = match self {
            RegisterValue::U32(self_value) => *self_value as u128,
            RegisterValue::U64(self_value) => *self_value as u128,
            RegisterValue::U128(self_value) => *self_value,
        };
        let other_value = match other {
            RegisterValue::U32(other_value) => *other_value as u128,
            RegisterValue::U64(other_value) => *other_value as u128,
            RegisterValue::U128(other_value) => *other_value,
        };
        self_value == other_value
    }
}

impl core::fmt::Display for RegisterValue {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            RegisterValue::U32(register_value) => write!(f, "{register_value:#010x}"),
            RegisterValue::U64(register_value) => write!(f, "{register_value:#018x}"),
            RegisterValue::U128(register_value) => write!(f, "{register_value:#034x}"),
        }
    }
}

impl From<u32> for RegisterValue {
    fn from(val: u32) -> Self {
        Self::U32(val)
    }
}

impl From<u64> for RegisterValue {
    fn from(val: u64) -> Self {
        Self::U64(val)
    }
}

impl From<u128> for RegisterValue {
    fn from(val: u128) -> Self {
        Self::U128(val)
    }
}

impl TryInto<u32> for RegisterValue {
    type Error = crate::Error;

    fn try_into(self) -> Result<u32, Self::Error> {
        match self {
            Self::U32(v) => Ok(v),
            Self::U64(v) => v
                .try_into()
                .map_err(|_| crate::Error::Other(anyhow!("Value '{}' too large for u32", v))),
            Self::U128(v) => v
                .try_into()
                .map_err(|_| crate::Error::Other(anyhow!("Value '{}' too large for u32", v))),
        }
    }
}

impl TryInto<u64> for RegisterValue {
    type Error = crate::Error;

    fn try_into(self) -> Result<u64, Self::Error> {
        match self {
            Self::U32(v) => Ok(v.into()),
            Self::U64(v) => Ok(v),
            Self::U128(v) => v
                .try_into()
                .map_err(|_| crate::Error::Other(anyhow!("Value '{}' too large for u64", v))),
        }
    }
}

impl TryInto<u128> for RegisterValue {
    type Error = crate::Error;

    fn try_into(self) -> Result<u128, Self::Error> {
        match self {
            Self::U32(v) => Ok(v.into()),
            Self::U64(v) => Ok(v.into()),
            Self::U128(v) => Ok(v),
        }
    }
}

/// Extension trait to support converting errors
/// from TryInto calls into [probe_rs::Error]
pub trait RegisterValueResultExt<T> {
    /// Convert [Result<T,E>] into `Result<T, probe_rs::Error>`
    fn into_crate_error(self) -> Result<T, Error>;
}

/// No translation conversion case
impl<T> RegisterValueResultExt<T> for Result<T, Error> {
    fn into_crate_error(self) -> Result<T, Error> {
        self
    }
}

/// Convert from Error = Infallible to Error = probe_rs::Error
impl<T> RegisterValueResultExt<T> for Result<T, Infallible> {
    fn into_crate_error(self) -> Result<T, Error> {
        Ok(self.unwrap())
    }
}

/// A static array of all the registers ([`CoreRegister`]).
#[derive(Debug, PartialEq)]
pub struct RegisterFile(Vec<CoreRegister>);

impl RegisterFile {
    /// Construct a new register file from a vector of [`CoreRegister`]s, and panics if it does not contain at least the essential entries.
    pub fn new(core_registers: Vec<CoreRegister>) -> RegisterFile {
        let register_file = RegisterFile(core_registers);
        // Ensure we have the minimum required registers.
        let _ = register_file.program_counter().unwrap();
        let _ = register_file.stack_pointer().unwrap();
        let _ = register_file.frame_pointer().unwrap();
        let _ = register_file.return_address().unwrap();
        register_file
    }

    /// Returns an iterator over the descriptions of all the core registers (non-FPU) of this core.
    pub fn core_registers(&self) -> impl Iterator<Item = &CoreRegister> {
        self.0.iter().filter(|r| {
            !matches!(
                r.role,
                Some(RegisterRole::FloatingPoint) | Some(RegisterRole::FloatingPointStatus)
            )
        })
    }

    /// Returns the nth platform register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn core_register(&self, index: usize) -> &CoreRegister {
        self.core_registers().nth(index).unwrap()
    }

    /// Returns the nth platform register if it is exists, `None` otherwise.
    pub fn get_core_register(&self, index: usize) -> Result<&CoreRegister, Error> {
        self.core_registers().nth(index).ok_or_else(|| {
            Error::GenericCoreError(format!(
                "Platform register {index:?} not found. Please report this as a bug."
            ))
        })
    }

    /// The frame pointer.
    pub fn frame_pointer(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::FramePointer))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No frame pointer found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// The program counter.
    pub fn program_counter(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::ProgramCounter))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No program counter found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// The stack pointer.
    pub fn stack_pointer(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::StackPointer))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No stack pointer found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// The link register.
    pub fn return_address(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::ReturnAddress))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No return address found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// Returns the nth argument register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn argument_register(&self, index: usize) -> &CoreRegister {
        self.get_argument_register(index).unwrap()
    }

    /// Returns the nth argument register if it is exists, `None` otherwise.
    pub fn get_argument_register(&self, index: usize) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .filter(|r| matches!(r.role, Some(RegisterRole::Argument(_))))
            .nth(index)
            .ok_or_else(|| {
                Error::GenericCoreError(format!(
                    "Argument register {index:?} not found. Please report this as a bug."
                ))
            })
    }

    /// The main stack pointer.
    pub fn msp(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::MainStackPointer))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No main stack pointer found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// The process stack pointer.
    pub fn psp(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::ProcessStackPointer))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No process stack pointer found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// The processor status register.
    pub fn psr(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::ProcessorStatus))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No processor status register found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// Other architecture specific registers
    pub fn other(&self) -> impl Iterator<Item = &CoreRegister> {
        self.0
            .iter()
            .filter(|r| matches!(r.role, Some(RegisterRole::Other(_))))
    }

    /// Find an architecture specific register by name
    pub fn other_by_name(&self, name: &str) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| matches!(r.role, Some(RegisterRole::Other(other_name)) if other_name == name))
            .ok_or_else(|| {
                Error::GenericCoreError(format!(
                    "Argument register {name:?} not found. Please report this as a bug."
                ))
            })
    }

    /// The fpu status register.
    pub fn fpsr(&self) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .find(|r| r.role == Some(RegisterRole::FloatingPointStatus))
            .ok_or_else(|| {
                Error::GenericCoreError(
                    "No FPU status register found. Please report this as a bug.".to_string(),
                )
            })
    }

    /// Returns an iterator over the descriptions of all the registers of this core.
    pub fn fpu_registers(&self) -> Option<impl Iterator<Item = &CoreRegister>> {
        let mut fpu_registers = self
            .0
            .iter()
            .filter(|r| r.role == Some(RegisterRole::FloatingPoint))
            .peekable();
        if fpu_registers.peek().is_some() {
            Some(fpu_registers)
        } else {
            None
        }
    }

    /// Returns the nth fpu register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn fpu_register(&self, index: usize) -> &CoreRegister {
        self.get_fpu_register(index).unwrap()
    }

    /// Returns the nth fpu register if it is exists, `None` otherwise.
    pub fn get_fpu_register(&self, index: usize) -> Result<&CoreRegister, Error> {
        self.0
            .iter()
            .filter(|r| r.role == Some(RegisterRole::FloatingPoint))
            .nth(index)
            .ok_or_else(|| {
                Error::GenericCoreError(format!(
                    "Argument register {index:?} not found. Please report this as a bug."
                ))
            })
    }
}
