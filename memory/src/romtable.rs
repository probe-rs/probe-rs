use log::{
    info,
    debug,
    error,
};
use enum_primitive_derive::Primitive;
use num_traits::cast::{
    FromPrimitive,
};

use coresight::{
    access_ports::{
        memory_ap::*,
        generic_ap::*,
    },
    access_ports,
    ap_access::*,
};


#[derive(Debug, PartialEq)]
pub enum RomTableError {
    Base,
    NotARomtable,
    AccessPortError(access_ports::AccessPortError),
    ComponentIdentificationError,
}

impl From<access_ports::AccessPortError> for RomTableError {
    fn from(e: access_ports::AccessPortError) -> Self {
        RomTableError::AccessPortError(e)
    }
}

#[derive(Debug)]
pub struct RomTableReader<'p, P: crate::MI> {
    base_address: u64,
    probe: &'p mut P,
}

impl<'p, P: crate::MI> RomTableReader<'p, P> {
    pub fn new(probe: &'p mut P, base_address: u64) -> Self {
        RomTableReader {
            base_address,
            probe,
        }
    }

    // TODO: Use an iterator here
    pub fn entries(&mut self) -> Result<Vec<RomTableEntryRaw>, RomTableError> {
        let mut entries = Vec::new();
        for component_offset in (0..0xfcc).step_by(4) {
            let component_address = self.base_address + component_offset;
            info!("Reading rom table entry at {:08x}", component_address);

            let mut entry_data = [0u32;1];
            self.probe.read_block(component_address as u32, &mut entry_data)?;

            // end of entries is marked by an all zero entry
            if entry_data[0] == 0 {
                info!("Entry consists of all zeroes, stopping.");
                break;
            }

            let entry_data = RomTableEntryRaw::new(self.base_address as u32, entry_data[0]);


            info!("ROM Table Entry: {:x?}", entry_data);

            entries.push(entry_data);
        }

        Ok(entries)
    }
}

#[derive(Debug, PartialEq)]
pub struct RomTable {
    entries: Vec<RomTableEntry>,
}

enum RomTableScanError {
    EndOfRomtable,
    SkipEntry,
    ReadFailed,
}

impl From<RomTableError> for RomTableScanError {
    fn from(e: RomTableError) -> Self {
        RomTableScanError::ReadFailed
    }
}

impl From<access_ports::AccessPortError> for RomTableScanError {
    fn from(e: access_ports::AccessPortError) -> Self {
        RomTableScanError::ReadFailed
    }
}

impl RomTable {
    pub fn try_parse<P>(link: &mut P, baseaddr: u64) -> Result<RomTable, RomTableError>
    where
        P:
            crate::MI
          + APAccess<GenericAP, IDR>
          + APAccess<MemoryAP, BASE>
          + APAccess<MemoryAP, BASE2>
    {
        info!("\tReading component data at: {:08x}", baseaddr);

        // Determine the component class to find out what component we are dealing with.
        let component_class = get_component_class(link, baseaddr)?;
        info!("\tComponent class: {:x?}", component_class);

        // Determine the peripheral id to find out what peripheral we are dealing with.
        let peripheral_id = get_peripheral_id(link, baseaddr)?;
        info!("\tComponent peripheral id: {:x?}", peripheral_id);

        if peripheral_id == PeripheralID::SystemControlSpace
        && component_class == ComponentClass::SystemControlSpace {
            // Found the scs (System Control Space) register of a cortex m0.
            info!("Found SCS CoreSight at address 0x{:08x}", baseaddr);
            
            // This means we can check the cpuid register (located at 0xe000_ed00).
            let cpu_id: u32 = link.read(0xe000_ed00)?;

            info!("CPUID: 0x{:08X}", cpu_id);
        }

        if ComponentClass::RomTable != component_class {
            return Err(RomTableError::NotARomtable);
        }

        // Iterate over all the available ROM table entries.
        let entries = (0..0xfcc)
            .step_by(4)
            .map(|component_offset| {
                // Read a single 32bit word which stands for a ROM table entry.
                info!("Reading rom table entry at {:08x}", baseaddr + component_offset);
                let mut entry_data = [0u32;1];
                link.read_block((baseaddr + component_offset) as u32, &mut entry_data);

                // The end of the ROM table is marked by an all zero entry.
                if entry_data[0] == 0 {
                    // We stop iterating as we have rached the end of the ROM table.
                    info!("ROM table entry consists of all zeroes, stopping.");
                    Err(RomTableScanError::EndOfRomtable)
                } else {
                    // We have found ourselves a valid ROM table entry and start working on it by parsing it.
                    let raw_entry = RomTableEntryRaw::new(baseaddr as u32, entry_data[0]);
                    info!("ROM Table Entry: {:x?}", raw_entry);

                    let entry_base_addr = raw_entry.component_addr();

                    Ok(RomTableEntry {
                        format: raw_entry.format,
                        power_domain_id: raw_entry.power_domain_id,
                        power_domain_valid: raw_entry.power_domain_valid,
                        rom_table: Some(
                            RomTable::try_parse(link, entry_base_addr as u64)
                                .map_err(|e| match e {
                                    RomTableError::Base => RomTableScanError::ReadFailed,
                                    RomTableError::NotARomtable => RomTableScanError::SkipEntry,
                                    RomTableError::AccessPortError(_) => RomTableScanError::ReadFailed,
                                    RomTableError::ComponentIdentificationError => RomTableScanError::ReadFailed,
                                })?
                            ),
                    })
                }
            })
            .take_while(|entry| match entry {
                Err(RomTableScanError::EndOfRomtable) => false,
                Err(RomTableScanError::ReadFailed) => false,
                _ => true,
            })
            .filter_map(|v| v.ok())
            .collect::<Vec<_>>();

        Ok(RomTable { entries })
    }
}

#[derive(Debug, PartialEq)]
pub struct RomTableEntryRaw {
    address_offset: i32,
    power_domain_id: u8,
    power_domain_valid: bool,
    format: bool,
    entry_present: bool,
    // Base address of the rom table
    base_addr: u32,         
}

impl  RomTableEntryRaw {
    fn new(base_addr: u32, raw: u32) -> Self {
        debug!("Parsing raw rom table entry: 0x{:05x}", raw);

        let address_offset = ((raw >> 12) & 0xf_ff_ff) as i32;
        let power_domain_id = ((raw >> 4) & 0xf) as u8;
        let power_domain_valid = (raw & 4) == 4;
        let format = (raw & 2) == 2;
        let entry_present = (raw & 1) == 1;

        RomTableEntryRaw {
            address_offset,
            power_domain_id,
            power_domain_valid,
            format,
            entry_present,
            base_addr,
        }
    }

    pub fn component_addr(&self) -> u32 {
        ((self.base_addr as i64) + ((self.address_offset << 12) as i64)) as u32
    }
}

#[derive(Debug, PartialEq)]
pub struct RomTableEntry {
    power_domain_id: u8,
    power_domain_valid: bool,
    format: bool,
    rom_table: Option<RomTable>,
}

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Primitive, Debug, PartialEq)]
enum ComponentClass {
    GenericVerificationComponent = 0,
    RomTable = 1,
    CoreSightComponent = 9,
    PeripheralTestBlock = 0xB,
    GenericIPComponent = 0xE,
    CoreLinkOrPrimeCellOrSystemComponent = 0xF,
}

impl ComponentClass {
    const SystemControlSpace: ComponentClass = ComponentClass::GenericIPComponent;
}

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Debug, PartialEq)]
enum Component {
    GenericVerificationComponent,
    Class1RomTable(RomTableEntryRaw),
    Class0RomTable,
    PeripheralTestBlock,
    GenericIPComponent,
    CoreLinkOrPrimeCellOrSystemComponent,
}

/// Try retrieve the component class.
/// The CIDR register is described in section D1.2.1 of the ADIv5.2 spec.
fn get_component_class<P>(link: &mut P, baseaddr: u64) -> Result<ComponentClass, RomTableError>
where
    P:
        crate::MI
{
    let mut data = [0u32;4];
    link.read_block(baseaddr as u32 | 0xFF0, &mut data)?;

    debug!("CIDR: {:x?}", data);

    if data[0] & 0xFF == 0x0D
        && data[1] & 0x0F == 0x00
        && data[2] & 0xFF == 0x05
        && data[3] & 0xFF == 0xB1 
    {
        FromPrimitive::from_u32((data[1] >> 4) & 0x0F).ok_or(RomTableError::ComponentIdentificationError)
    } else {
        error!("The CIDR registers did not contain the expected preambles.");
        Err(RomTableError::ComponentIdentificationError)
    }
}

#[derive(Debug, PartialEq)]
enum ComponentModification {
    No,
    Yes(u8),
}

#[allow(non_snake_case)]
#[derive(Debug, PartialEq)]
struct PeripheralID {
    pub REVAND: u8,
    pub CMOD: ComponentModification,
    pub REVISION: u8,
    pub JEDEC: bool,
    pub JEP106: jep106::JEP106Code,
    pub PART: u16,
    /// The SIZE is indicated as a multiple of 4k blocks the peripheral occupies.
    pub SIZE: u8
}

impl PeripheralID {
    const SystemControlSpace: PeripheralID = PeripheralID {
        REVAND: 0, CMOD: ComponentModification::No, REVISION: 0, JEDEC: true, JEP106: jep106::JEP106Code { cc: 0x4, id: 0x3B}, PART: 8, SIZE: 1
    };
}

impl PeripheralID {
    fn from_raw(data: &[u32;8]) -> Self {
        let jep106id = (((data[2] & 0x07) << 4) | ((data[1] >> 4) & 0x0F)) as u8;

        PeripheralID {
            REVAND: ((data[3] >> 4) & 0x0F) as u8,
            CMOD: match (data[3] & 0x0F) as u8 {
                0x0 => ComponentModification::No,
                v => ComponentModification::Yes(v),
            },
            REVISION: ((data[2] >> 4) & 0x0F) as u8,
            JEDEC: (data[2] & 0x8) > 1,
            JEP106: jep106::JEP106Code::new((data[4] & 0x0F) as u8, ((1 - jep106id.count_ones() as u8 % 2) << 7) | jep106id),
            PART: ((data[1] & 0x0F) | (data[0] & 0xFF)) as u16,
            SIZE: 2u32.pow((data[4] >> 4) & 0x0F) as u8
        }
    }
}

/// Try retrieve the peripheral id.
/// The CIDR register is described in section D1.2.2 of the ADIv5.2 spec.
fn get_peripheral_id<P>(link: &mut P, baseaddr: u64) -> Result<PeripheralID, RomTableError>
where
    P:
        crate::MI
{
    let mut data = [0u32;8];

    let peripheral_id_address = baseaddr + 0xFD0;

    debug!("Reading debug id from address: {:08x}", peripheral_id_address);

    link.read_block(baseaddr as u32 + 0xFD0, &mut data[4..])?;
    link.read_block(baseaddr as u32 + 0xFE0, &mut data[..4])?;

    debug!("Raw peripheral id: {:x?}", data);

    Ok(PeripheralID::from_raw(&data))
}

/// Retrieves the BASEADDR of a CoreSight component.
/// The layout of the BASE registers is defined in section C2.6.1 of the ADIv5.2 spec.
pub fn get_base_addr<P>(link: &mut P, port: u8) -> Option<u64>
    where
        P: APAccess<MemoryAP, BASE> + APAccess<MemoryAP, BASE2>
{
    // First we get the BASE register which lets us extract the BASEADDR required to access the romtable.
    let memory_port = MemoryAP::new(port);
    let base = match link.read_register_ap(memory_port, BASE::default()) {
        Ok(value) => value,
        Err(e) => { error!("Error reading the BASE registers: {:?}", e); return None },
    };
    info!("\n{:#x?}", base);

    let mut baseaddr = if let BaseaddrFormat::ADIv5 = base.Format {
        let base2 = match link.read_register_ap(memory_port, BASE2::default()) {
            Ok(value) => value,
            Err(e) => { error!("Error reading the BASE registers: {:?}", e); return None }
        };
        info!("\n{:x?}", base2);
        (u64::from(base2.BASEADDR) << 32)
    } else { 0 };
    baseaddr |= u64::from(base.BASEADDR << 12);

    info!("\nBASEADDR: {:x?}", baseaddr);
    Some(baseaddr)
}