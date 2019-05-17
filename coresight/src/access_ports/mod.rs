
#[macro_use]
mod register_generation;

pub mod generic_ap;
pub mod memory_ap;


use crate::ap_access::AccessPort;
use crate::common::Register;

#[derive(Debug, PartialEq)]
pub enum AccessPortError {
    ProbeError,
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
