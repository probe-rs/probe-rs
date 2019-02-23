use crate::access_ports::{
    APType,
    APRegister,
};

pub trait APAccess<PORT, REGISTER>
where
    PORT: APType,
    REGISTER: APRegister<PORT>,
{
    type Error;
    fn read_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<REGISTER, Self::Error>;
    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error>;
}