use crate::access_ports::{
    APRegister,
    GenericAP,
    IDR,
};

pub trait AccessPort {
    fn get_port_number(&self) -> u8;
}

pub trait APAccess<PORT, REGISTER>
where
    PORT: AccessPort,
    REGISTER: APRegister<PORT>,
{
    type Error;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<REGISTER, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error>;
}

/// Determine if an AP exists with the given AP number.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAP) -> bool
where
    AP: APAccess<GenericAP, IDR>
{
    if let Ok(idr) = debug_port.read_register_ap(access_port, IDR::default()) {
        u32::from(idr) != 0
    } else {
        false
    }
}