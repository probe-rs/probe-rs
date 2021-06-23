#[macro_use]
pub mod register_generation;
pub(crate) mod generic_ap;
pub(crate) mod memory_ap;

use crate::architecture::arm::dp::DebugPortError;
use crate::DebugProbeError;

pub use generic_ap::{ApClass, ApType, GenericAp, IDR};
pub use memory_ap::{
    AddressIncrement, BaseaddrFormat, DataSize, MemoryAp, BASE, BASE2, CSW, DRW, TAR,
};

use super::Register;

#[derive(Debug, thiserror::Error)]
pub enum AccessPortError {
    #[error("Failed to access address 0x{address:08x} as it is not aligned to the requirement of {alignment} bytes.")]
    MemoryNotAligned { address: u32, alignment: usize },
    #[error("Failed to read register {name} at address 0x{address:08x}")]
    RegisterReadError {
        address: u8,
        name: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Failed to write register {name} at address 0x{address:08x}")]
    RegisterWriteError {
        address: u8,
        name: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Out of bounds access")]
    OutOfBoundsError,
    #[error("Error while communicating with debug port")]
    DebugPort(#[from] DebugPortError),
    #[error("Failed to flush batched writes")]
    FlushError(#[from] DebugProbeError),
}

impl AccessPortError {
    pub fn register_read_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterReadError {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    pub fn register_write_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        source: E,
    ) -> Self {
        AccessPortError::RegisterWriteError {
            address: R::ADDRESS,
            name: R::NAME,
            source: Box::new(source),
        }
    }

    pub fn alignment_error(address: u32, alignment: usize) -> Self {
        AccessPortError::MemoryNotAligned { address, alignment }
    }
}

pub trait ApRegister<PORT: AccessPort>: Register + Sized {}

pub trait AccessPort {
    fn port_number(&self) -> u8;
}

/// Direct access to AP registers.
pub trait RawApAccess {
    type Error: std::error::Error + Send + Sync + 'static;

    /// Read a AP register.
    fn read_raw_ap_register(&mut self, port_number: u8, address: u8) -> Result<u32, Self::Error>;

    /// Read a register using a block transfer. This can be used
    /// to read multiple values from the same register.
    fn read_raw_ap_register_repeated(
        &mut self,
        port: u8,
        address: u8,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        for val in values {
            *val = self.read_raw_ap_register(port, address)?;
        }
        Ok(())
    }

    /// Write a AP register.
    fn write_raw_ap_register(
        &mut self,
        port: u8,
        address: u8,
        value: u32,
    ) -> Result<(), Self::Error>;

    /// Write a register using a block transfer. This can be used
    /// to write multiple values to the same register.
    fn write_raw_ap_register_repeated(
        &mut self,
        port: u8,
        address: u8,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        for val in values {
            self.write_raw_ap_register(port, address, *val)?;
        }
        Ok(())
    }
}

pub trait ApAccess {
    type Error: std::error::Error + Send + Sync + 'static;
    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Read a register using a block transfer. This can be used
    /// to read multiple values from the same register.
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;

    /// Write a register using a block transfer. This can be used
    /// to write multiple values to the same register.
    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>;
}

/*
impl<T> ApAccess for &mut T
where
    T: ApAccess,
{
    type Error = T::Error;

    fn read_ap_register<PORT, R>(&mut self, port: impl Into<PORT>) -> Result<R, Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        (*self).read_ap_register(port)
    }

    fn write_ap_register<PORT, R>(
        &mut self,
        port: impl Into<PORT>,
        register: R,
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        (*self).write_ap_register(port, register)
    }

    fn write_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        (*self).write_ap_register_repeated(port, register, values)
    }
    fn read_ap_register_repeated<PORT, R>(
        &mut self,
        port: impl Into<PORT> + Clone,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error>
    where
        PORT: AccessPort,
        R: ApRegister<PORT>,
    {
        (*self).read_ap_register_repeated(port, register, values)
    }
}
*/

/// Determine if an AP exists with the given AP number.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAp) -> bool
where
    AP: ApAccess,
{
    let idr_result: Result<IDR, AP::Error> = debug_port.read_ap_register(access_port);

    match idr_result {
        Ok(idr) => u32::from(idr) != 0,
        Err(_e) => false,
    }
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe.
/// Can fail silently under the hood testing an ap that doesnt exist and would require cleanup.
pub(crate) fn valid_access_ports<AP>(debug_port: &mut AP) -> Vec<GenericAp>
where
    AP: ApAccess,
{
    (0..=255)
        .map(GenericAp::new)
        .take_while(|port| access_port_is_valid(debug_port, *port))
        .collect::<Vec<GenericAp>>()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, f: P) -> Option<GenericAp>
where
    AP: ApAccess,
    P: Fn(IDR) -> bool,
{
    (0..=255).map(GenericAp::new).find(|ap| {
        if let Ok(idr) = debug_port.read_ap_register(*ap) {
            f(idr)
        } else {
            false
        }
    })
}
