pub mod memory_ap;

pub trait APType {
    fn get_port_number(&self) -> u8;
}
pub trait APRegister {
    fn to_u32(&self) -> u32;
    fn to_value<T: APValue>(&self) -> T;
}
pub trait APValue {
    fn from_u32(self, value: u32) -> Self;
    fn to_u32(&self) -> u32;
}