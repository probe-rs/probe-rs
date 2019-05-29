pub mod m0;

pub trait TargetRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

pub struct CoreRegisterAddress(u8);