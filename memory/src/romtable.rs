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

use std::cell::RefCell;


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
    probe: &'p RefCell<P>,
}

impl<'p, P: crate::MI> RomTableReader<'p, P> {
    pub fn new(probe: &'p RefCell<P>, base_address: u64) -> Self {
        RomTableReader {
            base_address,
            probe,
        }
    }

    /// Iterate over all entries of the rom table, non-recursively
    pub fn entries<'r>(&'r mut self) -> RomTableIterator<'p,'r,P> {
        RomTableIterator::new(self)
    }
}


pub struct RomTableIterator<'p, 'r, P: crate::MI> where 'r: 'p {
    rom_table_reader: &'r mut RomTableReader<'p, P>,
    offset: u64,
}

impl<'r,'p, P: crate::MI> RomTableIterator<'r, 'p, P> {
    pub fn new(reader: &'r mut RomTableReader<'p, P>) -> Self {
        RomTableIterator {
            rom_table_reader: reader,
            offset: 0,
        }
    }
}

impl<'p,'r, P: crate::MI> Iterator for RomTableIterator<'p,'r,P> {
    type Item = Result<RomTableEntryRaw, RomTableScanError>;

    fn next(&mut self) -> Option<Self::Item> {
        let component_address = self.rom_table_reader.base_address + self.offset;
        info!("Reading rom table entry at {:08x}", component_address);

        let mut probe = self.rom_table_reader.probe.borrow_mut();

        self.offset += 4;

        let mut entry_data = [0u32;1];
        if let Err(e) = probe.read_block(component_address as u32, &mut entry_data) {
            return Some(Err(e.into()));
        }

        // end of entries is marked by an all zero entry
        if entry_data[0] == 0 {
            info!("Entry consists of all zeroes, stopping.");
            return None
        }

        let entry_data = RomTableEntryRaw::new(
            self.rom_table_reader.base_address as u32, 
            entry_data[0]
        );


        //info!("ROM Table Entry: {:x?}", entry_data);
        Some(Ok(entry_data))
    }
}

#[derive(Debug, PartialEq)]
pub struct RomTable {
    id: ComponentId,
    entries: Vec<RomTableEntry>,
}

#[derive(Debug, PartialEq)]
pub enum RomTableScanError {
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
    pub fn try_parse<P>(link: &RefCell<P>, baseaddr: u64) -> Result<RomTable, RomTableError>
    where
        P:
            crate::MI
          + APAccess<GenericAP, IDR>
          + APAccess<MemoryAP, BASE>
          + APAccess<MemoryAP, BASE2>
    {
        

        info!("\tReading component data at: {:08x}", baseaddr);

        let component_id = ComponentInformationReader::new(baseaddr, link).read_all()?;

        // Determine the component class to find out what component we are dealing with.
        info!("\tComponent class: {:x?}", component_id.class);

        // Determine the peripheral id to find out what peripheral we are dealing with.
        info!("\tComponent peripheral id: {:x?}", component_id.peripheral_id);

        if component_id.peripheral_id == PeripheralID::SystemControlSpace
            && component_id.class == ComponentClass::SystemControlSpace {
            // Found the scs (System Control Space) register of a cortex m0.
            info!("Found SCS CoreSight at address 0x{:08x}", baseaddr);
            let mut borrowed_link = link.borrow_mut();
            
            // This means we can check the cpuid register (located at 0xe000_ed00).
            let cpu_id: u32 = borrowed_link.read(0xe000_ed00)?;

            info!("CPUID: 0x{:08X}", cpu_id);
        }

        if ComponentClass::RomTable != component_id.class {
            return Err(RomTableError::NotARomtable);
        }

        let mut reader = RomTableReader::new(&link, baseaddr);

        let entries = reader
            .entries()
            .filter_map(Result::ok)
            .map(|raw_entry| {
                let entry_base_addr = raw_entry.component_addr();

                RomTableEntry {
                    format: raw_entry.format,
                    power_domain_id: raw_entry.power_domain_id,
                    power_domain_valid: raw_entry.power_domain_valid,
                    rom_table: RomTable::try_parse(link, entry_base_addr as u64).ok()
                }
            })
            .collect();

        Ok(RomTable { 
            id: component_id,
            entries,
        })
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

impl RomTableEntryRaw {
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

/// Component Identification information 
/// 
/// Identification for a CoreSight component
#[derive(Debug, PartialEq)]
pub struct ComponentId {
    base_address: u64,
    class: ComponentClass,
    peripheral_id: PeripheralID,
}

pub struct ComponentInformationReader<'p, P: crate::MI> {
    base_address: u64,
    probe: &'p RefCell<P>,
}

impl<'p, P: crate::MI> ComponentInformationReader<'p, P> {
    pub fn new(base_address: u64, probe: &'p RefCell<P>) -> Self {
        ComponentInformationReader {
            base_address,
            probe
        }
    }

    pub fn component_class(&mut self) -> Result<ComponentClass, RomTableError> {
        let mut data = [0u32;4];
        let mut probe = self.probe.borrow_mut();

        probe.read_block(self.base_address as u32 + 0xFF0, &mut data)?;

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

    pub fn peripheral_id(&mut self) -> Result<PeripheralID, RomTableError> {
        let mut probe = self.probe.borrow_mut();

        let mut data = [0u32;8];

        let peripheral_id_address = self.base_address + 0xFD0;

        debug!("Reading debug id from address: {:08x}", peripheral_id_address);

        probe.read_block(self.base_address as u32 + 0xFD0, &mut data[4..])?;
        probe.read_block(self.base_address as u32 + 0xFE0, &mut data[..4])?;

        debug!("Raw peripheral id: {:x?}", data);

        Ok(PeripheralID::from_raw(&data))
    }

    pub fn read_all(&mut self) -> Result<ComponentId, RomTableError> {
        Ok(ComponentId {
            base_address: self.base_address,
            class: self.component_class()?,
            peripheral_id: self.peripheral_id()?,
        })
    }
}

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Primitive, Debug, PartialEq)]
pub enum ComponentClass {
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
    Class1RomTable,
    Class0RomTable,
    PeripheralTestBlock,
    GenericIPComponent,
    CoreLinkOrPrimeCellOrSystemComponent,
}


#[derive(Debug, PartialEq)]
pub enum ComponentModification {
    No,
    Yes(u8),
}

#[allow(non_snake_case)]
#[derive(Debug, PartialEq)]
/// Peripheral ID information for a CoreSight component
pub struct PeripheralID {
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
