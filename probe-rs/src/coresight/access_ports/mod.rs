#[macro_use]
mod register_generation;

pub mod custom_ap;
pub mod generic_ap;
pub mod memory_ap;

use crate::error::*;

use crate::coresight::ap_access::AccessPort;
use crate::coresight::common::Register;

impl Error {
    pub fn register_read_error<R: Register>(source: Error) -> Error {
        err!(
            RegisterRead {
                addr: R::ADDRESS,
                name: R::NAME,
            },
            source
        )
    }

    pub fn register_write_error<R: Register>(source: Error) -> Error {
        err!(
            RegisterWrite {
                addr: R::ADDRESS,
                name: R::NAME,
            },
            source
        )
    }
}

pub trait APRegister<PORT: AccessPort>: Register + Sized {
    const APBANKSEL: u8;
}
