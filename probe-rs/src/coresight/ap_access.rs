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
    type Error: std::error::Error;
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

    fn read_register_ap(
        &mut self,
        port: PORT,
        register: REGISTER,
    ) -> Result<REGISTER, Self::Error> {
        (*self).read_register_ap(port, register)
    }

    fn write_register_ap(&mut self, port: PORT, register: REGISTER) -> Result<(), Self::Error> {
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

/// Return a Vec of all valid access ports found that the target connected to the debug_probe
pub fn valid_access_ports<AP>(debug_port: &mut AP) -> Vec<GenericAP>
where
    AP: APAccess<GenericAP, IDR>,
{
    (0..=255)
        .map(GenericAP::new)
        .filter(|port| access_port_is_valid(debug_port, *port))
        .collect::<Vec<GenericAP>>()
}

/// Tries to find the first AP with the given idr value, returns `None` if there isn't any
pub fn get_ap_by_idr<AP, P>(debug_port: &mut AP, f: P) -> Option<GenericAP>
where
    AP: APAccess<GenericAP, IDR>,
    P: Fn(IDR) -> bool,
{
    (0..=255).map(GenericAP::new).find(|ap| {
        if let Ok(idr) = debug_port.read_register_ap(*ap, IDR::default()) {
            f(idr)
        } else {
            false
        }
    })
}
