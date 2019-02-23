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

// /// Determine if an AP exists with the given AP number.
// pub fn access_port_is_valid(debug_port: &mut DebugPort, access_port: AccessPortNumber) -> Result<bool, DebugPortError> {
//     let idr = debug_port.read_ap(((access_port as u32) << consts::APSEL_SHIFT) | consts::AP_IDR as u32)?;
//     Ok(idr != 0)
// }