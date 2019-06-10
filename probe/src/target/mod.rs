use crate::debug_probe::{
    MasterProbe,
    DebugProbeError,
    CpuInformation,
};

pub mod m0;

pub trait TargetRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

pub struct CoreRegisterAddress(u8);

impl From<CoreRegisterAddress> for u32 {
    fn from(value: CoreRegisterAddress) -> Self {
        u32::from(value.0)
    }
}

pub trait Target {
    fn halt(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>;

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    /// Steps one instruction and then enters halted state again.
    fn step(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>;

    fn read_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError>;

    fn write_core_reg(&self, mi: &mut MasterProbe, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError>;

    fn get_available_breakpoint_units(&self, mi: &mut MasterProbe) -> Result<u32, DebugProbeError>;

    fn enable_breakpoints(&self, mi: &mut MasterProbe, state: bool) -> Result<(), DebugProbeError>;

    fn set_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;

    fn enable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;

    fn disable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;
}