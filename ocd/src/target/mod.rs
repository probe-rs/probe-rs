pub mod info;

use serde::de::{Error, Unexpected};

use crate::{
    collection::get_core,
    probe::{
        debug_probe::{CpuInformation, DebugProbeError, MasterProbe},
        flash::memory::MemoryRegion,
    },
};

use std::fmt;

pub trait CoreRegister: Clone + From<u32> + Into<u32> + Sized + std::fmt::Debug {
    const ADDRESS: u32;
    const NAME: &'static str;
}

#[derive(Debug, Copy, Clone)]
pub struct CoreRegisterAddress(pub u8);

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
    pub R4: CoreRegisterAddress,
    pub R9: CoreRegisterAddress,
    pub PC: CoreRegisterAddress,
    pub LR: CoreRegisterAddress,
    pub SP: CoreRegisterAddress,
    pub XPSR: CoreRegisterAddress,
}

pub trait Core: std::fmt::Debug + objekt::Clone {
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    fn halt(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>;

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    fn reset(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    fn reset_and_halt(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    /// Steps one instruction and then enters halted state again.
    fn step(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>;

    fn read_core_reg(
        &self,
        mi: &mut MasterProbe,
        addr: CoreRegisterAddress,
    ) -> Result<u32, DebugProbeError>;

    fn write_core_reg(
        &self,
        mi: &mut MasterProbe,
        addr: CoreRegisterAddress,
        value: u32,
    ) -> Result<(), DebugProbeError>;

    fn get_available_breakpoint_units(&self, mi: &mut MasterProbe) -> Result<u32, DebugProbeError>;

    fn enable_breakpoints(&self, mi: &mut MasterProbe, state: bool) -> Result<(), DebugProbeError>;

    fn set_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;

    fn enable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;

    fn disable_breakpoint(&self, mi: &mut MasterProbe, addr: u32) -> Result<(), DebugProbeError>;

    fn read_block8(
        &self,
        mi: &mut MasterProbe,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), DebugProbeError>;

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses;
}

objekt::clone_trait_object!(Core);

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub name: String,
    pub manufacturer: jep106::JEP106Code,
    pub part: u16,
    pub flash_algorithm: Option<String>,
    pub memory_map: Vec<MemoryRegion>,
    pub core: Box<dyn Core>,
}

pub type TargetParseError = serde_yaml::Error;

impl Target {
    pub fn new(definition: &str) -> Result<Self, TargetParseError> {
        serde_yaml::from_str(definition)
    }
}

struct CoreVisitor;

impl<'de> serde::de::Visitor<'de> for CoreVisitor {
    type Value = Box<dyn Core>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "an existing core name")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some(core) = get_core(s) {
            Ok(core)
        } else {
            Err(Error::invalid_value(
                Unexpected::Other(&format!("Core {} does not exist.", s)),
                &self,
            ))
        }
    }
}

impl<'de> serde::Deserialize<'de> for Box<dyn Core> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(CoreVisitor)
    }
}

#[derive(Debug)]
pub enum TargetSelectionError {
    CouldNotAutodetect,
    TargetNotFound(String),
    TargetCouldNotBeParsed(TargetParseError),
}

impl fmt::Display for TargetSelectionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use TargetSelectionError::*;

        match self {
            CouldNotAutodetect => write!(f, "Target could not be automatically identified."),
            TargetNotFound(ref t) => write!(f, "Failed to find target defintion for target {}", t),
            TargetCouldNotBeParsed(ref e) => {
                write!(f, "Failed to parse target definition for target: ")?;
                e.fmt(f)
            }
        }
    }
}

impl std::error::Error for TargetSelectionError {}

impl From<TargetParseError> for TargetSelectionError {
    fn from(error: TargetParseError) -> Self {
        TargetSelectionError::TargetCouldNotBeParsed(error)
    }
}
