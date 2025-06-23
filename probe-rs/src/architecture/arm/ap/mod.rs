//! Defines types and registers for ADIv5 and ADIv6 access ports (APs).

pub(crate) mod generic_ap;
pub(crate) mod memory_ap;
mod registers;
pub mod v1;
pub mod v2;

pub use generic_ap::GenericAp;
pub use memory_ap::MemoryAp;
pub use memory_ap::MemoryApType;
pub(crate) use registers::define_ap_register;
pub use registers::{BASE, BASE2, BD0, BD1, BD2, BD3, CFG, CSW, DRW, IDR, MBT, TAR, TAR2};

use crate::architecture::arm::{
    ArmError, DapAccess, DebugPortError, FullyQualifiedApAddress, RegisterParseError,
};

use crate::probe::DebugProbeError;

/// Sum-type of the Memory Access Ports.
#[derive(Debug)]
pub enum AccessPort {
    /// Any memory Access Port.
    // TODO: Allow each memory by types to be specialised with there specific feature
    MemoryAp(memory_ap::MemoryAp),
    /// Other Access Ports not used for memory accesses.
    Other(GenericAp),
}
impl AccessPortType for AccessPort {
    fn ap_address(&self) -> &FullyQualifiedApAddress {
        match self {
            AccessPort::MemoryAp(mem_ap) => mem_ap.ap_address(),
            AccessPort::Other(o) => o.ap_address(),
        }
    }
}

/// Some error during AP handling occurred.
#[derive(Debug, thiserror::Error)]
pub enum AccessPortError {
    /// An error occurred when trying to read a register.
    #[error("Failed to read register {name} at address {address:#04x}")]
    RegisterRead {
        /// The address of the register.
        address: u64,
        /// The name if the register.
        name: &'static str,
        /// The underlying root error of this access error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// An error occurred when trying to write a register.
    #[error("Failed to write register {name} at address {address:#04x}")]
    RegisterWrite {
        /// The address of the register.
        address: u64,
        /// The name if the register.
        name: &'static str,
        /// The underlying root error of this access error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// Some error with the operation of the APs DP occurred.
    #[error("Error while communicating with debug port")]
    DebugPort(#[from] DebugPortError),
    /// An error occurred when trying to flush batched writes of to the AP.
    #[error("Failed to flush batched writes")]
    Flush(#[from] DebugProbeError),

    /// Error while parsing a register
    #[error("Error parsing a register")]
    RegisterParse(#[from] RegisterParseError),
}

impl AccessPortError {
    /// Constructs a [`AccessPortError::RegisterRead`] from just the source error and the register type.
    pub fn register_read_error<R: ApRegister, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterRead {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    /// Constructs a [`AccessPortError::RegisterWrite`] from just the source error and the register type.
    pub fn register_write_error<R: ApRegister, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterWrite {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }
}

/// A trait to be implemented by ports types providing access to a register.
pub trait ApRegAccess<Reg: ApRegister>: AccessPortType {}

/// A trait to be implemented on access port types.
pub trait AccessPortType {
    /// Returns the address of the access port.
    fn ap_address(&self) -> &FullyQualifiedApAddress;
}

/// A trait to be implemented by access port drivers to implement access port operations.
pub trait ApAccess {
    /// Read a register of the access port.
    fn read_ap_register<PORT, R>(&mut self, port: &PORT) -> Result<R, ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister;

    /// Read a register of the access port using a block transfer.
    /// This can be used to read multiple values from the same register.
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister;

    /// Write a register of the access port.
    fn write_ap_register<PORT, R>(&mut self, port: &PORT, register: R) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister;

    /// Write a register of the access port using a block transfer.
    /// This can be used to write multiple values to the same register.
    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister;
}

impl<T: DapAccess> ApAccess for T {
    #[tracing::instrument(skip(self, port), fields(ap = port.ap_address().ap_v1().ok(), register = R::NAME, value))]
    fn read_ap_register<PORT, R>(&mut self, port: &PORT) -> Result<R, ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister,
    {
        let raw_value = self.read_raw_ap_register(port.ap_address(), R::ADDRESS)?;

        tracing::Span::current().record("value", raw_value);

        tracing::debug!("Register read succesful");

        Ok(raw_value.try_into()?)
    }

    fn write_ap_register<PORT, R>(&mut self, port: &PORT, register: R) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister,
    {
        tracing::debug!("Writing AP register {}, value={:x?}", R::NAME, register);
        self.write_raw_ap_register(port.ap_address(), R::ADDRESS, register.into())
            .inspect_err(|err| tracing::warn!("Failed to write AP register {}: {}", R::NAME, err))
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &[u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister,
    {
        tracing::debug!(
            "Writing register {}, block with len={} words",
            R::NAME,
            values.len(),
        );
        self.write_raw_ap_register_repeated(port.ap_address(), R::ADDRESS, values)
    }

    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: &PORT,
        values: &mut [u32],
    ) -> Result<(), ArmError>
    where
        PORT: AccessPortType + ApRegAccess<R> + ?Sized,
        R: ApRegister,
    {
        tracing::debug!(
            "Reading register {}, block with len={} words",
            R::NAME,
            values.len(),
        );

        self.read_raw_ap_register_repeated(port.ap_address(), R::ADDRESS, values)
    }
}

/// The unit of data that is transferred in one transfer via the DRW commands.
///
/// This can be configured with the CSW command.
///
/// ALL MCUs support `U32`. All other transfer sizes are optionally implemented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DataSize {
    /// 1 byte transfers are supported.
    U8 = 0b000,
    /// 2 byte transfers are supported.
    U16 = 0b001,
    /// 4 byte transfers are supported.
    #[default]
    U32 = 0b010,
    /// 8 byte transfers are supported.
    U64 = 0b011,
    /// 16 byte transfers are supported.
    U128 = 0b100,
    /// 32 byte transfers are supported.
    U256 = 0b101,
}

impl DataSize {
    pub(crate) fn to_byte_count(self) -> usize {
        match self {
            DataSize::U8 => 1,
            DataSize::U16 => 2,
            DataSize::U32 => 4,
            DataSize::U64 => 8,
            DataSize::U128 => 16,
            DataSize::U256 => 32,
        }
    }
}

/// Invalid data size.
pub struct InvalidDataSizeError;

impl TryFrom<u8> for DataSize {
    type Error = InvalidDataSizeError;
    fn try_from(value: u8) -> Result<Self, InvalidDataSizeError> {
        match value {
            0b000 => Ok(DataSize::U8),
            0b001 => Ok(DataSize::U16),
            0b010 => Ok(DataSize::U32),
            0b011 => Ok(DataSize::U64),
            0b100 => Ok(DataSize::U128),
            0b101 => Ok(DataSize::U256),
            _ => Err(InvalidDataSizeError),
        }
    }
}

/// The increment to the TAR that is performed after each DRW read or write.
///
/// This can be used to avoid successive TAR transfers for writes of consecutive addresses.
/// This will effectively save half the bandwidth!
///
/// Can be configured in the CSW.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AddressIncrement {
    /// No increments are happening after the DRW access. TAR always stays the same.
    /// Always supported.
    Off = 0b00,
    /// Increments the TAR by the size of the access after each DRW access.
    /// Always supported.
    #[default]
    Single = 0b01,
    /// Enables packed access to the DRW (see C2.2.7).
    /// Only available if sub-word access is supported by the core.
    Packed = 0b10,
}

impl AddressIncrement {
    /// Create a new `AddressIncrement` from a u8.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0b00 => Some(AddressIncrement::Off),
            0b01 => Some(AddressIncrement::Single),
            0b10 => Some(AddressIncrement::Packed),
            _ => None,
        }
    }
}

/// The format of the BASE register (see C2.6.1).
#[derive(Debug, PartialEq, Eq, Clone, Copy, Default)]
pub enum BaseAddrFormat {
    /// The legacy format of very old cores. Very little cores use this.
    #[default]
    Legacy = 0,
    /// The format all newer MCUs use.
    ADIv5 = 1,
}

/// Describes the class of an access port defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApClass {
    /// This describes a custom AP that is vendor defined and not defined by ARM
    #[default]
    Undefined = 0b0000,
    /// The standard ARM COM-AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
    ComAp = 0b0001,
    /// The standard ARM MEM-AP defined  in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification
    MemAp = 0b1000,
}

impl ApClass {
    /// Tries to create an `ApClass` from a given `u8`.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0b0000 => Some(ApClass::Undefined),
            0b0001 => Some(ApClass::ComAp),
            0b1000 => Some(ApClass::MemAp),
            _ => None,
        }
    }
}

/// The type of AP defined in the [`ARM Debug Interface v5.2`](https://developer.arm.com/documentation/ihi0031/f/?lang=en) specification.
/// You can find the details in the table C1-2 on page C1-146.
/// The different types correspond to the different access/memory buses of ARM cores.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ApType {
    /// This is the most basic AP that is included in most MCUs and uses SWD or JTAG as an access bus.
    #[default]
    JtagComAp = 0x0,
    /// A AMBA based AHB3 AP (see E1.5).
    AmbaAhb3 = 0x1,
    /// A AMBA based APB2 and APB3 AP (see E1.8).
    AmbaApb2Apb3 = 0x2,
    /// A AMBA based AXI3 and AXI4 AP (see E1.2).
    AmbaAxi3Axi4 = 0x4,
    /// A AMBA based AHB5 AP (see E1.6).
    AmbaAhb5 = 0x5,
    /// A AMBA based APB4 and APB5 AP (see E1.9).
    AmbaApb4Apb5 = 0x6,
    /// A AMBA based AXI5 AP (see E1.4).
    AmbaAxi5 = 0x7,
    /// A AMBA based AHB5 AP with enhanced HPROT (see E1.7).
    AmbaAhb5Hprot = 0x8,
}

impl ApType {
    /// Tries to create an `ApType` from a given `u8`.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x0 => Some(ApType::JtagComAp),
            0x1 => Some(ApType::AmbaAhb3),
            0x2 => Some(ApType::AmbaApb2Apb3),
            0x4 => Some(ApType::AmbaAxi3Axi4),
            0x5 => Some(ApType::AmbaAhb5),
            0x6 => Some(ApType::AmbaApb4Apb5),
            0x7 => Some(ApType::AmbaAxi5),
            0x8 => Some(ApType::AmbaAhb5Hprot),
            _ => None,
        }
    }
}
/// Base trait for all versions of access port registers
pub trait ApRegister:
    Clone + TryFrom<u32, Error = RegisterParseError> + Into<u32> + Sized + std::fmt::Debug
{
    /// The address of the register (in bytes).
    const ADDRESS: u64;

    /// The name of the register as string.
    const NAME: &'static str;
}
