use crate::coresight::{
    access_ports::{
        generic_ap::{APClass, IDR},
        memory_ap::{BaseaddrFormat, MemoryAP, BASE, BASE2},
    },
    ap_access::{valid_access_ports, APAccess},
    memory::romtable::{CSComponent, CSComponentId, PeripheralID, RomTableError},
};
use crate::probe::{DebugProbeError, MasterProbe};
use colored::*;
use jep106::JEP106Code;
use log::debug;
use std::{error::Error, fmt};

#[derive(Debug)]
pub enum ReadError {
    DebugProbeError(DebugProbeError),
    RomTableError(RomTableError),
    NotFound,
}

impl From<DebugProbeError> for ReadError {
    fn from(e: DebugProbeError) -> Self {
        ReadError::DebugProbeError(e)
    }
}

impl From<RomTableError> for ReadError {
    fn from(e: RomTableError) -> Self {
        ReadError::RomTableError(e)
    }
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReadError::DebugProbeError(e) => write!(f, "failed to access target: {}", e),
            ReadError::RomTableError(e) => write!(f, "failed to parse ROM table: {}", e),
            ReadError::NotFound => f.write_str("chip info not found in IDR"),
        }
    }
}

impl Error for ReadError {}

#[derive(Debug)]
pub struct ChipInfo {
    pub manufacturer: JEP106Code,
    pub part: u16,
}

impl ChipInfo {
    pub fn read_from_rom_table(probe: &mut MasterProbe) -> Result<Self, ReadError> {
        for access_port in valid_access_ports(probe) {
            let idr = probe.read_ap_register(access_port, IDR::default())?;
            debug!("{:#x?}", idr);

            if idr.CLASS == APClass::MEMAP {
                let access_port: MemoryAP = access_port.into();

                let base_register = probe.read_ap_register(access_port, BASE::default())?;

                let mut baseaddr = if BaseaddrFormat::ADIv5 == base_register.Format {
                    let base2 = probe.read_ap_register(access_port, BASE2::default())?;
                    (u64::from(base2.BASEADDR) << 32)
                } else {
                    0
                };
                baseaddr |= u64::from(base_register.BASEADDR << 12);

                let component_table = CSComponent::try_parse(&probe.into(), baseaddr as u64)?;

                match component_table {
                    CSComponent::Class1RomTable(
                        CSComponentId {
                            peripheral_id:
                                PeripheralID {
                                    JEP106: Some(jep106),
                                    PART: part,
                                    ..
                                },
                            ..
                        },
                        ..,
                    ) => {
                        return Ok(ChipInfo {
                            manufacturer: jep106,
                            part,
                        });
                    }
                    _ => continue,
                }
            }
        }
        log::info!(
            "{}\n{}\n{}\n{}",
            "If you are using a Nordic chip, it might be locked to debug access".yellow(),
            "Run cargo flash with --nrf-recover to unlock".yellow(),
            "WARNING: --nrf-recover will erase the entire code".yellow(),
            "flash and UICR area of the device, in addition to the entire RAM".yellow()
        );

        Err(ReadError::NotFound)
    }
}

impl fmt::Display for ChipInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let manu = match self.manufacturer.get() {
            Some(name) => name.to_string(),
            None => format!(
                "<unknown manufacturer (cc={:2x}, id={:2x})>",
                self.manufacturer.cc, self.manufacturer.id
            ),
        };
        write!(f, "{} 0x{:04x}", manu, self.part)
    }
}
