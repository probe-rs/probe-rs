use crate::debug_port::{
    DPRegister,
};

pub trait DebugPort {
    fn version(&self) -> &'static str;
}

pub trait DPAccess<PORT, REGISTER>
where
    PORT: DebugPort,
    REGISTER: DPRegister<PORT>,
{
    type Error: std::fmt::Debug;
    fn read_dp_register(&mut self, port: &PORT) -> Result<REGISTER, Self::Error>;

    fn write_dp_register(&mut self, port: &PORT, register: REGISTER) -> Result<(), Self::Error>;
}