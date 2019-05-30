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

#[derive(Copy, Clone)]
pub struct Target {
    pub halt: fn(mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>,

    pub run: fn(mi: &mut MasterProbe) -> Result<(), DebugProbeError>,

    /// Steps one instruction and then enters halted state again.
    pub step: fn(mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>,

    pub read_core_reg: fn(mi: &mut MasterProbe, addr: CoreRegisterAddress) -> Result<u32, DebugProbeError>,

    pub write_core_reg: fn(mi: &mut MasterProbe, addr: CoreRegisterAddress, value: u32) -> Result<(), DebugProbeError>,

    pub get_available_breakpoint_units: fn(mi: &mut MasterProbe) -> Result<u32, DebugProbeError>,

    pub enable_breakpoints: fn(mi: &mut MasterProbe, state: bool) -> Result<(), DebugProbeError>,

    pub set_breakpoint: fn(mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>,

    pub enable_breakpoint: fn(mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>,

    pub disable_breakpoint: fn(mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>,
}