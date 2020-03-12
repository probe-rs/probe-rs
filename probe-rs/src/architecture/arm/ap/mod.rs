#[macro_use]
mod register_generation;
pub(crate) mod custom_ap;
pub(crate) mod generic_ap;
pub(crate) mod memory_ap;

use crate::architecture::arm::dp::DebugPortError;

pub use generic_ap::{APClass, GenericAP, IDR};
pub(crate) use memory_ap::mock;
pub use memory_ap::{
    AddressIncrement, BaseaddrFormat, DataSize, MemoryAP, BASE, BASE2, CSW, DRW, TAR,
};

use super::Register;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AccessPortError {
    #[error("Failed to access address 0x{0:08x} as it is not aligned to 4 bytes.")]
    MemoryNotAligned(u32),
    #[error("Failed to read register {name}, address 0x{address:08x}")]
    RegisterReadError {
        address: u8,
        name: &'static str,
        inner: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Failed to write register {name}, address 0x{address:08x}")]
    RegisterWriteError {
        address: u8,
        name: &'static str,
        inner: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("Out of bounds access")]
    OutOfBoundsError,
    #[error("Error while communicating with debug port: {0}")]
    DebugPort(#[from] DebugPortError),
}

impl AccessPortError {
    pub fn register_read_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        inner: E,
    ) -> AccessPortError {
        AccessPortError::RegisterReadError {
            address: R::ADDRESS,
            name: R::NAME,
            inner: Box::new(inner),
        }
    }

    pub fn register_write_error<R: Register, E: std::error::Error + Send + Sync + 'static>(
        inner: E,
    ) -> AccessPortError {
        AccessPortError::RegisterWriteError {
            address: R::ADDRESS,
            name: R::NAME,
            inner: Box::new(inner),
        }
    }
}

pub trait APRegister<PORT: AccessPort>: Register + Sized {
    const APBANKSEL: u8;
}

pub trait AccessPort {
    fn get_port_number(&self) -> u8;
}

pub trait APAccess<PORT, R>
where
    PORT: AccessPort,
    R: APRegister<PORT>,
{
    type Error: std::error::Error + Send + Sync + 'static;
    fn read_ap_register(&mut self, port: PORT, register: R) -> Result<R, Self::Error>;

    /// Read a register using a block transfer. This can be used
    /// to read multiple values from the same register.
    fn read_ap_register_repeated(
        &mut self,
        port: PORT,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error>;

    fn write_ap_register(&mut self, port: PORT, register: R) -> Result<(), Self::Error>;

    /// Write a register using a block transfer. This can be used
    /// to write multiple values to the same register.
    fn write_ap_register_repeated(
        &mut self,
        port: PORT,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error>;
}

impl<'a, T, PORT, R> APAccess<PORT, R> for &'a mut T
where
    T: APAccess<PORT, R>,
    PORT: AccessPort,
    R: APRegister<PORT>,
{
    type Error = T::Error;

    fn read_ap_register(&mut self, port: PORT, register: R) -> Result<R, Self::Error> {
        (*self).read_ap_register(port, register)
    }

    fn write_ap_register(&mut self, port: PORT, register: R) -> Result<(), Self::Error> {
        (*self).write_ap_register(port, register)
    }

    fn write_ap_register_repeated(
        &mut self,
        port: PORT,
        register: R,
        values: &[u32],
    ) -> Result<(), Self::Error> {
        (*self).write_ap_register_repeated(port, register, values)
    }
    fn read_ap_register_repeated(
        &mut self,
        port: PORT,
        register: R,
        values: &mut [u32],
    ) -> Result<(), Self::Error> {
        (*self).read_ap_register_repeated(port, register, values)
    }
}

/// Determine if an AP exists with the given AP number.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAP) -> bool
where
    AP: APAccess<GenericAP, IDR>,
{
    if let Ok(idr) = debug_port.read_ap_register(access_port, IDR::default()) {
        u32::from(idr) != 0
    } else {
        false
    }
}

/// Return a Vec of all valid access ports found that the target connected to the debug_probe
pub fn valid_access_ports<AP>(debug_port: &mut AP) -> Vec<GenericAP>
where
    AP: APAccess<GenericAP, IDR>,
{
    (0..=255)
        .map(GenericAP::new)
        .take_while(|port| access_port_is_valid(debug_port, *port))
        .collect::<Vec<GenericAP>>()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, f: P) -> Option<GenericAP>
where
    AP: APAccess<GenericAP, IDR>,
    P: Fn(IDR) -> bool,
{
    (0..=255).map(GenericAP::new).find(|ap| {
        if let Ok(idr) = debug_port.read_ap_register(*ap, IDR::default()) {
            f(idr)
        } else {
            false
        }
    })
}
