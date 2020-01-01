pub mod info;

use serde::de::{Error, Unexpected};

use crate::{
    cores::get_core,
    probe::{DebugProbeError, MasterProbe},
};

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

#[derive(Debug, Clone)]
pub struct CoreInformation {
    pub pc: u32,
}

pub trait Core: std::fmt::Debug + dyn_clone::DynClone {
    /// Wait until the core is halted. If the core does not halt on its own,
    /// a [`DebugProbeError::Timeout`] error will be returned.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    fn wait_for_core_halted(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    /// Try to halt the core. This function ensures the core is actually halted, and
    /// returns a [`DebugProbeError::Timeout`] otherwise.
    ///
    /// [`DebugProbeError::Timeout`]: ../probe/debug_probe/enum.DebugProbeError.html#variant.Timeout
    fn halt(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError>;

    fn run(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    /// Reset the core, and then continue to execute instructions. If the core
    /// should be halted after reset, use the [`reset_and_halt`] function.
    ///
    /// [`reset_and_halt`]: trait.Core.html#tymethod.reset_and_halt
    fn reset(&self, mi: &mut MasterProbe) -> Result<(), DebugProbeError>;

    /// Reset the core, and then immediately halt. To continue execution after
    /// reset, use the [`reset`] function.
    ///
    /// [`reset`]: trait.Core.html#tymethod.reset
    fn reset_and_halt(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError>;

    /// Steps one instruction and then enters halted state again.
    fn step(&self, mi: &mut MasterProbe) -> Result<CoreInformation, DebugProbeError>;

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

    fn set_breakpoint(
        &self,
        mi: &mut MasterProbe,
        bp_unit_index: usize,
        addr: u32,
    ) -> Result<(), DebugProbeError>;

    fn clear_breakpoint(
        &self,
        mi: &mut MasterProbe,
        bp_unit_index: usize,
    ) -> Result<(), DebugProbeError>;

    fn read_block8(
        &self,
        mi: &mut MasterProbe,
        address: u32,
        data: &mut [u8],
    ) -> Result<(), DebugProbeError>;

    fn registers<'a>(&self) -> &'a BasicRegisterAddresses;
}

dyn_clone::clone_trait_object!(Core);

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
