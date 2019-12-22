use super::debug_port::DPRegister;
use crate::error::Result;

pub trait DebugPort {
    fn version(&self) -> &'static str;
}

pub trait DPAccess<PORT, REGISTER>
where
    PORT: DebugPort,
    REGISTER: DPRegister<PORT>,
{
    fn read_dp_register(&mut self, port: &PORT) -> Result<REGISTER>;

    fn write_dp_register(&mut self, port: &PORT, register: REGISTER) -> Result<()>;
}
