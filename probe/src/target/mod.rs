pub mod m0;
pub mod nrf51822;

pub use m0::*;

use crate::flash::{
    flasher::FlashAlgorithm,
    memory::{
        MemoryRegion,
    }
};
use crate::debug_probe::{
    MasterProbe,
    DebugProbeError,
    CpuInformation,
};

pub trait CoreRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

#[derive(Debug, Copy, Clone)]
pub struct CoreRegisterAddress(u8);

impl From<CoreRegisterAddress> for u32 {
    fn from(value: CoreRegisterAddress) -> Self {
        u32::from(value.0)
    }
}

impl From<u8> for CoreRegisterAddress {
    fn from(value: u8) -> Self {
        CoreRegisterAddress(value)
    }
}

#[allow(non_snake_case)]
#[derive(Copy, Clone)]
pub struct BasicRegisterAddresses {
    pub R0: CoreRegisterAddress,
    pub R1: CoreRegisterAddress,
    pub R2: CoreRegisterAddress,
    pub R3: CoreRegisterAddress,
    pub R9: CoreRegisterAddress,
    pub PC: CoreRegisterAddress,
    pub LR: CoreRegisterAddress,
    pub SP: CoreRegisterAddress,
}

pub trait Core {
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;
    
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

    fn read_block8(&self, mi: &mut MasterProbe, address: u32, data: &mut [u8]) -> Result<(), DebugProbeError>;
}

#[derive(Clone)]
pub struct TargetInfo {
    pub flash_algorithm: FlashAlgorithm,
    pub basic_register_addresses: BasicRegisterAddresses,
    pub memory_map: Vec<MemoryRegion>,
}

pub struct Target {
    pub core: Box<dyn Core>,
    pub info: TargetInfo,
}

impl Target {
    pub fn new(core: impl Core + 'static, info: TargetInfo) -> Self {
        Self {
            core: Box::new(core),
            info: info,
        }
    }
}