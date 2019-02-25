#[macro_use]
pub mod register_generation;
pub mod generic_ap;
pub mod memory_ap;

use crate::ap_access::AccessPort;
use crate::common::Register;

#[derive(Debug)]
pub enum AccessPortError {
    ProbeError,
    InvalidAccessPortNumber,
    MemoryNotAligned,
}

pub trait APRegister<PORT: AccessPort>: Register + Sized {
    const APBANKSEL: u8;
}