use crate::probe::debug_probe::MasterProbe;
use crate::{
    coresight::{
        access_ports::{
            generic_ap::{APClass, IDR},
            memory_ap::{BaseaddrFormat, MemoryAP, BASE, BASE2},
        },
        ap_access::{valid_access_ports, APAccess},
    },
    memory::romtable::{CSComponent, CSComponentId, PeripheralID},
};
use jep106::JEP106Code;

pub struct ChipInfo {
    pub manufacturer: JEP106Code,
    pub part: u16,
}

impl ChipInfo {
    pub fn read_from_rom_table(probe: &mut MasterProbe) -> Option<Self> {
        for access_port in valid_access_ports(probe) {
            let idr = probe.read_register_ap(access_port, IDR::default()).ok()?;
            println!("{:#x?}", idr);

            if idr.CLASS == APClass::MEMAP {
                let access_port: MemoryAP = access_port.into();

                let base_register = probe.read_register_ap(access_port, BASE::default()).ok()?;

                let mut baseaddr = if BaseaddrFormat::ADIv5 == base_register.Format {
                    let base2 = probe.read_register_ap(access_port, BASE2::default()).ok()?;
                    (u64::from(base2.BASEADDR) << 32)
                } else {
                    0
                };
                baseaddr |= u64::from(base_register.BASEADDR << 12);

                let component_table = CSComponent::try_parse(&probe.into(), baseaddr as u64);

                match component_table.ok()? {
                    CSComponent::Class1RomTable(
                        CSComponentId {
                            peripheral_id:
                                PeripheralID {
                                    JEP106: jep106,
                                    PART: part,
                                    ..
                                },
                            ..
                        },
                        ..,
                    ) => {
                        return Some(ChipInfo {
                            manufacturer: jep106?,
                            part,
                        })
                    }
                    _ => continue,
                }
            }
        }

        None
    }
}
