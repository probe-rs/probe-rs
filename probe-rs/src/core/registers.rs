//! Core registers are represented by the `CoreRegister` struct, and collected in a `RegisterFile` for each of the supported architectures.

use crate::Error;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Ordering,
    convert::Infallible,
    fmt::{Display, Formatter},
};

/// The type of data stored in a register, with size in bits encapsulated in the enum.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum RegisterDataType {
    /// Unsigned integer data, with size in bits encapsulated.
    UnsignedInteger(usize),
    /// Floating point data, with size in bits encapsulated.
    FloatingPoint(usize),
}

/// This is used to label the register with a specific role that it plays during program execution and exception handling.
/// This denotes the purpose of a register (e.g. `return address`),
/// while the [`CoreRegister::name`] will contain the architecture specific label of the register.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RegisterRole {
    /// The default role for a register, with the name as defined by the architecture.
    Core(&'static str),
    /// Function Argument registers like "A0", "a1", etc. (uses architecture specific names)
    Argument(&'static str),
    /// Function Return value registers like "R0", "r1", etc. (uses architecture specific names)
    Return(&'static str),
    /// Program Counter register
    ProgramCounter,
    /// Frame Pointer register
    FramePointer,
    /// Stack Pointer register
    StackPointer,
    /// Main Stack Pointer register
    MainStackPointer,
    /// Process Stack Pointer register
    ProcessStackPointer,
    /// Processor Status register
    ProcessorStatus,
    /// Return Address register
    ReturnAddress,
    /// Floating Point Unit register
    FloatingPoint,
    /// Floating Point Status register
    FloatingPointStatus,
    /// Other architecture specific roles, e.g. "saved", "temporary", "variable", etc.
    Other(&'static str),
}

impl Display for RegisterRole {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            RegisterRole::Core(name) => write!(f, "{}", name),
            RegisterRole::Argument(name) => write!(f, "{}", name),
            RegisterRole::Return(name) => write!(f, "{}", name),
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

/// The rule used to preserve the value of a register between function calls during unwinding,
/// when DWARF unwind information is not available.
///
/// The rules for these are based on the 'Procedure Calling Standard' for each of the supported architectures:
/// - Implemented: [AAPCS32](https://github.com/ARM-software/abi-aa/blob/main/aapcs32/aapcs32.rst#core-registers)
/// - To be Implemented: [AAPCS64](https://github.com/ARM-software/abi-aa/blob/main/aapcs32/aapcs32.rst#core-registers)
/// - To be Implemented: [RISC-V PCS](https://github.com/riscv-non-isa/riscv-elf-psabi-doc/releases/download/v1.0/riscv-abi.pdf)
///
/// Please note that the `Procedure Calling Standard` define register rules for the act of calling and/or returning from functions,
/// while the timing of a stack unwinding is different (the `callee` has not yet completed / executed the epilogue),
/// and the rules about preserving register values have to take this into account.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnwindRule {
    /// Callee-saved, a.k.a non-volatile registers, or call-preserved.
    /// If there is DWARF unwind `RegisterRule` we will apply it during unwind,
    /// otherwise we assume it was untouched and preserve the current value.
    Preserve,
    /// Caller-saved, a.k.a. volatile registers, or call-clobbered.
    /// If there is DWARF unwind `RegisterRule` we will apply it during unwind,
    /// otherwise we assume it was corrupted by the callee, and clear the value.
    /// Note: This is the default value, and is used for all situations where DWARF unwind
    /// information is not available, and the register is not explicitly marked in the definition.
    #[default]
    Clear,
    /// Additional rules are required to determine the value of the register.
    /// These are typically found in either the DWARF unwind information,
    /// or requires additional platform specific registers to be read.
    SpecialRule,
}

/// Describes a core (or CPU / hardware) register with its properties.
/// Each architecture will have a set of general purpose registers, and potentially some special purpose registers. It also happens that some general purpose registers can be used for special purposes. For instance, some ARM variants allows the `LR` (link register / return address) to be used as general purpose register `R14`."
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CoreRegister {
    // /// Some architectures have multiple names for the same register, depending on the context and the role of the register.
    pub(crate) id: RegisterId,
    /// If the register plays a special role (one or more) during program execution and exception handling, this array will contain the appropriate [`RegisterRole`] entry/entries.
    pub(crate) roles: &'static [RegisterRole],
    pub(crate) data_type: RegisterDataType,
    /// For unwind purposes (debug and/or exception handling), we need to know how values are preserved between function calls. (Applies to ARM and RISC-V)
    #[serde(skip_serializing)]
    pub unwind_rule: UnwindRule,
}

impl PartialOrd for CoreRegister {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.id.cmp(&other.id))
    }
}

impl Ord for CoreRegister {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}

impl Display for CoreRegister {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let primary_name = self.name();
        write!(f, "{}", primary_name)?;
        if !self.roles.is_empty() {
            for role in self.roles {
                if primary_name != role.to_string() {
                    write!(f, "/{}", role)?;
                }
            }
        }
        Ok(())
    }
}

impl CoreRegister {
    /// Get the primary display name (As defined by `RegisterRole::Core()` of this register
    pub fn name(&self) -> &'static str {
        self.roles
            .iter()
            .find_map(|role| match role {
                RegisterRole::Core(name) => Some(*name),
                _ => None,
            })
            .unwrap_or("Unknown")
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
        match self.data_type() {
            RegisterDataType::UnsignedInteger(size_in_bits) => size_in_bits,
            RegisterDataType::FloatingPoint(size_in_bits) => size_in_bits,
        }
    }

    /// Get the size, in bytes, of this register
    pub fn size_in_bytes(&self) -> usize {
        // Always round up
        self.size_in_bits().div_ceil(8)
    }

    /// Get the width to format this register as a hex string
    /// Assumes a format string like `{:#0<width>x}`
    pub fn format_hex_width(&self) -> usize {
        (self.size_in_bytes() * 2) + 2
    }

    /// Helper method to identify registers that have a specific role in its definition.
    pub fn register_has_role(&self, role: RegisterRole) -> bool {
        for r in self.roles {
            if r == &role {
                return true;
            }
        }
        false
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
#[derive(Debug, Copy, Clone, PartialEq, PartialOrd, Ord, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum RegisterValue {
    /// 32-bit unsigned integer
    U32(u32),
    /// 64-bit unsigned integer
    U64(u64),
    /// 128-bit unsigned integer, often used with SIMD / FP
    U128(u128),
}

impl RegisterValue {
    /// Safely increment an address by a fixed number of bytes.
    pub fn increment_address(&mut self, bytes: usize) -> Result<(), Error> {
        match self {
            RegisterValue::U32(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u32) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
            RegisterValue::U64(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u64) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
            RegisterValue::U128(value) => {
                if let Some(reg_val) = value.checked_add(bytes as u128) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to add {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
        }
    }

    /// Safely decrement an address by a fixed number of bytes.
    pub fn decrement_address(&mut self, bytes: usize) -> Result<(), Error> {
        match self {
            RegisterValue::U32(value) => {
                if let Some(reg_val) = value.checked_sub(bytes as u32) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to subtract {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
            RegisterValue::U64(value) => {
                if let Some(reg_val) = value.checked_sub(bytes as u64) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to subtract {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
            RegisterValue::U128(value) => {
                if let Some(reg_val) = value.checked_sub(bytes as u128) {
                    *value = reg_val;
                    Ok(())
                } else {
                    Err(Error::Other(format!(
                        "Overflow error: Attempting to subtract {} bytes to Register value {}",
                        bytes, self
                    )))
                }
            }
        }
    }

    /// Determine if the contained register value is equal to the maximum value that can be stored in that datatype.
    pub fn is_max_value(&self) -> bool {
        match self {
            RegisterValue::U32(register_value) => *register_value == u32::MAX,
            RegisterValue::U64(register_value) => *register_value == u64::MAX,
            RegisterValue::U128(register_value) => *register_value == u128::MAX,
        }
    }

    /// Determine if the contained register value is zero.
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
                .map_err(|_| crate::Error::Other(format!("Value '{}' too large for u32", v))),
            Self::U128(v) => v
                .try_into()
                .map_err(|_| crate::Error::Other(format!("Value '{}' too large for u32", v))),
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
                .map_err(|_| crate::Error::Other(format!("Value '{}' too large for u64", v))),
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
/// from TryInto calls into [Error]
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

/// A static array of all the registers ([`CoreRegister`]) that apply to a specific architecture.
#[derive(Debug, PartialEq)]
pub struct CoreRegisters(Vec<&'static CoreRegister>);

impl CoreRegisters {
    /// Construct a new register file from a vector of &[`CoreRegister`]s.
    /// The register file must contain at least the essential entries for program counter, stack pointer, frame pointer and return address registers.
    pub fn new(core_registers: Vec<&'static CoreRegister>) -> CoreRegisters {
        CoreRegisters(core_registers)
    }

    /// Returns an iterator over the descriptions of all the non-FPU registers of this core.
    pub fn core_registers(&self) -> impl Iterator<Item = &CoreRegister> {
        self.0
            .iter()
            .filter(|r| {
                !r.roles.iter().any(|role| {
                    matches!(
                        role,
                        RegisterRole::FloatingPoint | RegisterRole::FloatingPointStatus
                    )
                })
            })
            .cloned()
    }

    /// Returns an iterator over the descriptions of all the registers of this core.
    pub fn all_registers(&self) -> impl Iterator<Item = &CoreRegister> {
        self.0.iter().cloned()
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
    pub fn get_core_register(&self, index: usize) -> Option<&CoreRegister> {
        self.core_registers().nth(index)
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
    pub fn get_argument_register(&self, index: usize) -> Option<&CoreRegister> {
        self.0
            .iter()
            .filter(|r| {
                r.roles
                    .iter()
                    .any(|role| matches!(role, RegisterRole::Argument(_)))
            })
            .cloned()
            .nth(index)
    }

    /// Returns the nth result register.
    ///
    /// # Panics
    ///
    /// Panics if the register at given index does not exist.
    pub fn result_register(&self, index: usize) -> &CoreRegister {
        self.get_result_register(index).unwrap()
    }

    /// Returns the nth result register if it is exists, `None` otherwise.
    pub fn get_result_register(&self, index: usize) -> Option<&CoreRegister> {
        self.0
            .iter()
            .filter(|r| {
                r.roles
                    .iter()
                    .any(|role| matches!(role, RegisterRole::Return(_)))
            })
            .cloned()
            .nth(index)
    }

    /// The program counter.
    pub fn pc(&self) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| r.register_has_role(RegisterRole::ProgramCounter))
            .cloned()
    }

    /// The main stack pointer.
    pub fn msp(&self) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| r.register_has_role(RegisterRole::MainStackPointer))
            .cloned()
    }

    /// The process stack pointer.
    pub fn psp(&self) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| r.register_has_role(RegisterRole::ProcessStackPointer))
            .cloned()
    }

    /// The processor status register.
    pub fn psr(&self) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| r.register_has_role(RegisterRole::ProcessorStatus))
            .cloned()
    }

    /// Find any register that have a `RegisterRole::Other` and the specified name.
    pub fn other_by_name(&self, name: &str) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| {
                r.roles
                    .iter()
                    .any(|role| matches!(role, RegisterRole::Other(n) if *n == name))
            })
            .cloned()
    }

    /// The fpu status register.
    pub fn fpsr(&self) -> Option<&CoreRegister> {
        self.0
            .iter()
            .find(|r| r.register_has_role(RegisterRole::FloatingPointStatus))
            .cloned()
    }

    /// Returns an iterator over the descriptions of all the registers of this core.
    pub fn fpu_registers(&self) -> Option<impl Iterator<Item = &CoreRegister>> {
        let mut fpu_registers = self
            .0
            .iter()
            .filter(|r| r.register_has_role(RegisterRole::FloatingPoint))
            .peekable();
        if fpu_registers.peek().is_some() {
            Some(fpu_registers.cloned())
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
    pub fn get_fpu_register(&self, index: usize) -> Option<&CoreRegister> {
        self.0
            .iter()
            .filter(|r| r.register_has_role(RegisterRole::FloatingPoint))
            .cloned()
            .nth(index)
    }
}
