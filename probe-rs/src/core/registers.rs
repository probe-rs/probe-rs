//! Core registers are represented by the [RegisterDescription] struct, and collected in a [RegisterFile] for each of the supported architectures.

use crate::Error;
use anyhow::{anyhow, Result};
use std::{cmp::Ordering, convert::Infallible};

/// The type of data stored in a register
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegisterDataType {
    /// Unsigned integer data
    UnsignedInteger,
    /// Floating point data
    FloatingPoint,
}

/// Describes a register with its properties.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisterDescription {
    pub(crate) name: &'static str,
    pub(crate) _kind: RegisterKind,
    pub(crate) id: RegisterId,
    pub(crate) _type: RegisterDataType,
    pub(crate) size_in_bits: usize,
}

impl RegisterDescription {
    /// Get the display name of this register
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// Get the type of data stored in this register
    pub fn data_type(&self) -> RegisterDataType {
        self._type.clone()
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

impl From<RegisterDescription> for RegisterId {
    fn from(description: RegisterDescription) -> RegisterId {
        description.id
    }
}

impl From<&RegisterDescription> for RegisterId {
    fn from(description: &RegisterDescription) -> RegisterId {
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

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum RegisterKind {
    General,
    PC,
    Fp,
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

/// Register description for a core.
#[derive(Debug, PartialEq)]
pub struct RegisterFile {
    pub(crate) platform_registers: &'static [RegisterDescription],

    /// Register description for the program counter
    pub(crate) program_counter: &'static RegisterDescription,

    pub(crate) stack_pointer: &'static RegisterDescription,

    pub(crate) return_address: &'static RegisterDescription,

    pub(crate) frame_pointer: &'static RegisterDescription,

    pub(crate) argument_registers: &'static [RegisterDescription],

    pub(crate) result_registers: &'static [RegisterDescription],

    pub(crate) msp: Option<&'static RegisterDescription>,

    pub(crate) psp: Option<&'static RegisterDescription>,

    pub(crate) psr: Option<&'static RegisterDescription>,

    pub(crate) fp_status: Option<&'static RegisterDescription>,

    pub(crate) fp_registers: Option<&'static [RegisterDescription]>,

    pub(crate) other: &'static [RegisterDescription],
}

impl RegisterFile {
    /// Returns an iterator over the descriptions of all the "platform" registers of this core.
    pub fn platform_registers(&self) -> impl Iterator<Item = &RegisterDescription> {
        self.platform_registers.iter()
    }

    /// The frame pointer.
    pub fn frame_pointer(&self) -> &RegisterDescription {
        self.frame_pointer
    }

    /// The program counter.
    pub fn program_counter(&self) -> &RegisterDescription {
        self.program_counter
    }

    /// The stack pointer.
    pub fn stack_pointer(&self) -> &RegisterDescription {
        self.stack_pointer
    }

    /// The link register.
    pub fn return_address(&self) -> &RegisterDescription {
        self.return_address
    }

    /// Returns the nth argument register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn argument_register(&self, index: usize) -> &RegisterDescription {
        &self.argument_registers[index]
    }

    /// Returns the nth argument register if it is exists, `None` otherwise.
    pub fn get_argument_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.argument_registers.get(index)
    }

    /// Returns the nth result register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn result_register(&self, index: usize) -> &RegisterDescription {
        &self.result_registers[index]
    }

    /// Returns the nth result register if it is exists, `None` otherwise.
    pub fn get_result_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.result_registers.get(index)
    }

    /// Returns the nth platform register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn platform_register(&self, index: usize) -> &RegisterDescription {
        &self.platform_registers[index]
    }

    /// Returns the nth platform register if it is exists, `None` otherwise.
    pub fn get_platform_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.platform_registers.get(index)
    }

    /// The main stack pointer.
    pub fn msp(&self) -> Option<&RegisterDescription> {
        self.msp
    }

    /// The process stack pointer.
    pub fn psp(&self) -> Option<&RegisterDescription> {
        self.psp
    }

    /// The processor status register.
    pub fn psr(&self) -> Option<&RegisterDescription> {
        self.psr
    }

    /// Other architecture specific registers
    pub fn other(&self) -> impl Iterator<Item = &RegisterDescription> {
        self.other.iter()
    }

    /// Find an architecture specific register by name
    pub fn other_by_name(&self, name: &str) -> Option<&RegisterDescription> {
        self.other.iter().find(|r| r.name == name)
    }

    /// The fpu status register.
    pub fn fpscr(&self) -> Option<&RegisterDescription> {
        self.fp_status
    }

    /// Returns an iterator over the descriptions of all the registers of this core.
    pub fn fpu_registers(&self) -> Option<impl Iterator<Item = &RegisterDescription>> {
        self.fp_registers.map(|r| r.iter())
    }

    /// Returns the nth fpu register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn fpu_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.fp_registers.map(|r| &r[index])
    }

    /// Returns the nth fpu register if it is exists, `None` otherwise.
    pub fn get_fpu_register(&self, index: usize) -> Option<&RegisterDescription> {
        self.fp_registers.and_then(|r| r.get(index))
    }
}
