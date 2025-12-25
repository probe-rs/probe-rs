//! This module enclouses ETM configurations. ETM or embedded trace macrocell
//! is the module responsible for real-time unintrusive trace generation.

mod etmv4;

use crate::architecture::arm::memory::romtable::{CoresightComponent, PeripheralType};
use crate::architecture::arm::{ArmDebugInterface, ArmError};
use etmv4::{EtmV4, EtmV4Config, EtmV4Decoder, EtmV4Packet};

use super::find_component;

/// Represents a ETM packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EtmPacket {
    /// ETMv4 packet type.
    V4(EtmV4Packet),
}

/// Encompasses the varying ETM decoder types.
#[derive(Clone, Debug)]
pub enum EtmDecoder {
    /// ETMv4 decoder type.
    V4(EtmV4Decoder),
}

impl EtmDecoder {
    /// Constructs a new [`EtmDecoder`] instance
    pub fn new(etm: &mut Etm) -> Result<Self, ArmError> {
        match etm {
            Etm::V4(_) => Ok(etm.decoder()?.clone()),
        }
    }

    /// Feeds a data slice into the decoder.
    pub fn feed(&mut self, data: &[u8]) -> Vec<EtmPacket> {
        match self {
            EtmDecoder::V4(decoder) => decoder.feed(data),
        }
    }
}

/// A struct that encompasses ETM. Any ETM version should be added separately.
pub enum Etm<'a> {
    /// ETMv4 module.
    V4(EtmV4<'a>),
}

impl<'a> Etm<'a> {
    /// Loads the module according to its [`PeripheralType`]
    pub fn load(
        interface: &'a mut dyn ArmDebugInterface,
        components: &'a [CoresightComponent],
    ) -> Result<Self, ArmError> {
        let component = find_component(components, PeripheralType::Etm)?;

        let periph_id = component.component.id().peripheral_id();

        // ETMv4 is known from its arch_id. Other ETM types should be checked for
        // other identifiers.
        match periph_id.arch_id() {
            // This is ETMv4.
            0x4A13 => Ok(Etm::V4(EtmV4::new(interface, component))),
            _ => Err(ArmError::Other("ETM version is not supported.".to_string())),
        }
    }

    /// Configure the selected ETM for the instruction trace.
    pub fn enable_instruction_trace(&mut self) -> Result<(), ArmError> {
        match self {
            Etm::V4(etm) => etm.enable_instruction_trace(&EtmV4Config::default()),
        }
    }

    /// Returns the decoder instance of the loaded ETM.
    pub fn decoder(&mut self) -> Result<EtmDecoder, ArmError> {
        match self {
            Etm::V4(etm) => Ok(EtmDecoder::V4(etm.decoder()?)),
        }
    }
}
