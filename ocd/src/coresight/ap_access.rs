use super::access_ports::{
    generic_ap::{GenericAP, IDR},
    APRegister,
};

pub trait AccessPort {
    fn get_port_number(&self) -> u8;
}

pub trait APAccess<PORT, REGISTER>
where
    PORT: AccessPort,
    REGISTER: APRegister<PORT>,
{
    type Error: std::fmt::Debug;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER)
        -> Result<REGISTER, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error>;
}

impl<'a, T, PORT, REGISTER> APAccess<PORT, REGISTER> for &'a mut T
where
    T: APAccess<PORT, REGISTER>,
    PORT: AccessPort,
    REGISTER: APRegister<PORT>,
{
    type Error = T::Error;

    fn read_register_ap(&mut self, port: PORT, register: REGISTER)
        -> Result<REGISTER, Self::Error>
    {
        (*self).read_register_ap(port, register)
    }

    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error>
    {
        (*self).write_register_ap(port, register)
    }
}

/// Determine if an AP exists with the given AP number.
pub fn access_port_is_valid<AP>(debug_port: &mut AP, access_port: GenericAP) -> bool
where
    AP: APAccess<GenericAP, IDR>,
{
    if let Ok(idr) = debug_port.read_register_ap(access_port, IDR::default()) {
        u32::from(idr) != 0
    } else {
        false
    }
}
