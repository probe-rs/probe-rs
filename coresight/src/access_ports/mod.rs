pub mod memory_ap;

use crate::common::Register;

#[derive(Debug)]
pub enum AccessPortError {
    ProbeError,
    InvalidAccessPortNumber,
    MemoryNotAligned,
}

pub trait APType {
    fn get_port_number(&self) -> u8;
}

pub trait APRegister<PORT: APType>: Register + Sized {
    const APBANKSEL: u8;
}