pub mod memory_ap;

pub trait APType {
    fn get_port_number(&self) -> u8;
}
pub trait APRegister<T: APValue> {
    fn to_u16(&self) -> u16;
    fn get_value(&self, value: u32) -> T;
    fn get_apbanksel(&self) -> u8;
}
pub trait APValue {
    fn from_u32(self, value: u32) -> Self;
    fn to_u32(&self) -> u32;
}