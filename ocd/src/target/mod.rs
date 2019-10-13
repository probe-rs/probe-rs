use std::collections::HashMap;
use std::fs::read_to_string;
use serde::de::{
    Error,
    Unexpected,
};

use crate::{
    probe::{
        flash::{
            flasher::FlashAlgorithm,
            memory::{
                MemoryRegion,
            },
        },
        debug_probe::{
            MasterProbe,
            DebugProbeError,
            CpuInformation,
        },
    },
    collection::get_core,
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
}

pub trait Core: std::fmt::Debug + objekt::Clone {
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;
    
    fn halt(&self, mi: &mut MasterProbe) -> Result<CpuInformation, DebugProbeError>;

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    fn reset(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

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

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses;
}

objekt::clone_trait_object!(Core);

#[derive(Debug, Clone, Deserialize)]
pub struct Target {
    pub name: String,
    pub flash_algorithm: FlashAlgorithmContainer,
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
            Err(Error::invalid_value(Unexpected::Other(&format!("Core {} does not exist.", s)), &self))
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
            CouldNotAutodetect => 
                write!(f, "Target could not be automatically identified."),
            TargetNotFound(ref t) =>
                write!(f, "Failed to find target defintion for target {}", t),
            TargetCouldNotBeParsed(ref e) => {
                write!(f, "Failed to parse target definition for target: ")?;
                e.fmt(f)
            }
        }
    }
}

impl std::error::Error for TargetSelectionError { }

impl From<TargetParseError> for TargetSelectionError {
    fn from(error: TargetParseError) -> Self {
        TargetSelectionError::TargetCouldNotBeParsed(error)
    }
}

pub fn identify_target() -> Result<Target, TargetSelectionError> {
    // TODO: Poll this from the connected target. For now return nRF51.
    Err(TargetSelectionError::CouldNotAutodetect)
}

#[derive(Debug, Clone)]
pub struct FlashAlgorithmContainer {
    path: String,
    cache: Option<FlashAlgorithm>,
}

impl FlashAlgorithmContainer {
    pub fn internal(&mut self, registry: &HashMap<&'static str, &'static str>) -> Option<&FlashAlgorithm> {
        if self.cache.is_none() {
            dbg!(registry);
            dbg!(self.path.as_str());
            self.cache = registry
                .get(self.path.as_str())
                .and_then(|definition| {
                    match FlashAlgorithm::new(definition) {
                        Ok(algorithm) => Some(algorithm),
                        Err(error) => { log::error!("{:?}", error); None },
                    }
                });
            dbg!(&self.cache);
        }

        self.cache.as_ref()
    }

    pub fn external(&mut self) -> Option<&FlashAlgorithm> {
        if self.cache.is_none() {
            let string = read_to_string(& self.path);
            self.cache = match string {
                Ok(definition) => match FlashAlgorithm::new(definition.as_str()) {
                    Ok(algorithm) => Some(algorithm),
                    Err(error) => { log::error!("{:?}", error); None },
                },
                Err(error) => { log::error!("{:?}", error); None },
            }
        }
        
        self.cache.as_ref()
    }

    pub fn get(&self) -> Option<&FlashAlgorithm> {
        self.cache.as_ref()
    }
}

struct FlashAlgorithmVisitor;

impl<'de> serde::de::Visitor<'de> for FlashAlgorithmVisitor {
    type Value = FlashAlgorithmContainer;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a path to an existing an algorithm description file")
    }

    fn visit_str<E>(self, s: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        // TODO: Maybe validate path somehow.
        Ok(FlashAlgorithmContainer {
            path: s.to_owned(),
            cache: None,
        })
    }
}

impl<'de> serde::Deserialize<'de> for FlashAlgorithmContainer {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_identifier(FlashAlgorithmVisitor)
    }
}