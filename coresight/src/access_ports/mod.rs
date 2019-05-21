
#[macro_use]
mod register_generation;

pub mod generic_ap;
pub mod memory_ap;

use std::error::Error;
use std::fmt;

use crate::ap_access::AccessPort;
use crate::common::Register;

#[derive(Debug, PartialEq)]
pub enum AccessPortError {
    InvalidAccessPortNumber,
    MemoryNotAligned,
    RegisterReadError{
        addr: u8,
        name: &'static str,
    },
    RegisterWriteError{
        addr: u8,
        name: &'static str,
    },
}

impl Error for AccessPortError { }

impl fmt::Display for AccessPortError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use AccessPortError::*;

        match self {
            InvalidAccessPortNumber => write!(f, "Invalid Access Port Number"),
            MemoryNotAligned => write!(f, "Misaligned memory access"),
            RegisterReadError { addr, name } => write!(f, "Failed to read register {}, address 0x{:08x}", name, addr),
            RegisterWriteError { addr, name } => write!(f, "Failed to write register {}, address 0x{:08x}", name, addr),
        }
    }
}

impl AccessPortError {
    pub fn register_read_error<R: Register>() -> AccessPortError {
        AccessPortError::RegisterReadError {
            addr: R::ADDRESS,
            name: R::NAME,
        }
    }

    pub fn register_write_error<R: Register>() -> AccessPortError {
        AccessPortError::RegisterWriteError {
            addr: R::ADDRESS,
            name: R::NAME,
        }
    }
}

pub trait APRegister<PORT: AccessPort>: Register + Sized {
    const APBANKSEL: u8;
}
