use crate::common::Register;

pub mod memory_ap;

pub trait APType {
    fn get_port_number(&self) -> u8;
}

pub trait APRegister<PORT: APType>: Register + Sized {
    const APBANKSEL: u8;
}