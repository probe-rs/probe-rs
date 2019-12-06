use crate::coresight::{
    access_ports,
    access_ports::{generic_ap::*, memory_ap::*},
    ap_access::*,
    memory::MI,
};
use enum_primitive_derive::Primitive;
use log::{debug, info, warn};
use num_traits::cast::FromPrimitive;
use std::cell::RefCell;
use std::error::Error;
use std::fmt;

#[derive(Debug, PartialEq)]
pub enum RomTableError {
    NotARomtable,
    AccessPortError(access_ports::AccessPortError),
    CSComponentIdentificationError,
}

impl Error for RomTableError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            RomTableError::AccessPortError(ref e) => Some(e),
            _ => None,
        }
    }
}

impl fmt::Display for RomTableError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use RomTableError::*;

        match self {
            NotARomtable => write!(f, "Component is not a valid rom table"),
            AccessPortError(ref e) => e.fmt(f),
            CSComponentIdentificationError => write!(f, "Failed to identify CoreSight component"),
        }
    }
}

impl From<access_ports::AccessPortError> for RomTableError {
    fn from(e: access_ports::AccessPortError) -> Self {
        RomTableError::AccessPortError(e)
    }
}

#[derive(Debug)]
pub struct RomTableReader<'p, P: MI> {
    base_address: u64,
    probe: &'p RefCell<P>,
}

/// Iterates over a ROM table non recursively.
impl<'p, P: MI> RomTableReader<'p, P> {
    pub fn new(probe: &'p RefCell<P>, base_address: u64) -> Self {
        RomTableReader {
            base_address,
            probe,
        }
    }

    /// Iterate over all entries of the rom table, non-recursively
    pub fn entries<'r>(&'r mut self) -> RomTableIterator<'p, 'r, P> {
        RomTableIterator::new(self)
    }
}

pub struct RomTableIterator<'p, 'r, P: MI>
where
    'r: 'p,
{
    rom_table_reader: &'r mut RomTableReader<'p, P>,
    offset: u64,
}

impl<'r, 'p, P: MI> RomTableIterator<'r, 'p, P> {
    pub fn new(reader: &'r mut RomTableReader<'p, P>) -> Self {
        RomTableIterator {
            rom_table_reader: reader,
            offset: 0,
        }
    }
}

impl<'p, 'r, P: MI> Iterator for RomTableIterator<'p, 'r, P> {
    type Item = Result<RomTableEntryRaw, RomTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        let component_address = self.rom_table_reader.base_address + self.offset;
        info!("Reading rom table entry at {:08x}", component_address);

        let mut probe = self.rom_table_reader.probe.borrow_mut();

        self.offset += 4;

        let mut entry_data = [0u32; 1];

        if let Err(e) = probe.read_block32(component_address as u32, &mut entry_data) {
            return Some(Err(e.into()));
        }

        // end of entries is marked by an all zero entry
        if entry_data[0] == 0 {
            info!("Entry consists of all zeroes, stopping.");
            return None;
        }

        let entry_data =
            RomTableEntryRaw::new(self.rom_table_reader.base_address as u32, entry_data[0]);

        //info!("ROM Table Entry: {:x?}", entry_data);
        Some(Ok(entry_data))
    }
}

/// Encapsulates information about a CoreSight component.
#[derive(Debug, PartialEq)]
pub struct RomTable {
    entries: Vec<RomTableEntry>,
}

impl RomTable {
    /// Tries to parse a CoreSight component table.
    ///
    /// This does not check whether the data actually signalizes
    /// to contain a ROM table but assumes this was checked beforehand.
    pub fn try_parse<P>(link: &RefCell<P>, base_address: u64) -> RomTable
    where
        P: MI + APAccess<GenericAP, IDR> + APAccess<MemoryAP, BASE> + APAccess<MemoryAP, BASE2>,
    {
        RomTable {
            entries: RomTableReader::new(&link, base_address)
                .entries()
                .filter_map(Result::ok)
                .filter_map(|raw_entry| {
                    let entry_base_addr = raw_entry.component_addr();
                    if !raw_entry.entry_present {
                        return None;
                    }

                    if let Ok(component_data) =
                        CSComponent::try_parse(link, u64::from(entry_base_addr))
                    {
                        Some(RomTableEntry {
                            format: raw_entry.format,
                            power_domain_id: raw_entry.power_domain_id,
                            power_domain_valid: raw_entry.power_domain_valid,
                            component_data,
                        })
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>(),
        }
    }
}

/// A ROM table entry with raw information parsed.
///
/// Described in section D3.4.4 of the ADIv5.2 specification.
///
/// This should only be used for parsing the raw memory structures of the entry.
///
/// For advanced usages, see [RomTableEntry](struct.RomTableEntry.html).
#[derive(Debug, PartialEq)]
pub struct RomTableEntryRaw {
    /// The offset from the BASEADDR at which the CoreSight component
    /// behind this ROM table entry is located.
    address_offset: i32,
    /// The power domain ID of the CoreSight component behind the ROM table entry.
    power_domain_id: u8,
    /// The power domain is valid if this is true.
    power_domain_valid: bool,
    /// Reads one if the ROM table has 32bit format.
    ///
    /// It is unsure if it can have a RAZ value.
    format: bool,
    /// Indicates whether the ROM table behind the address offset is present.
    pub entry_present: bool,
    // Base address of the rom table
    base_addr: u32,
}

impl RomTableEntryRaw {
    /// Create a new RomTableEntryRaw from a ROM table entry.
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

    /// Returns the address of the CoreSight component behind a ROM table entry.
    pub fn component_addr(&self) -> u32 {
        (i64::from(self.base_addr) + (i64::from(self.address_offset << 12))) as u32
    }
}

#[derive(Debug, PartialEq)]
pub struct RomTableEntry {
    power_domain_id: u8,
    power_domain_valid: bool,
    format: bool,
    component_data: CSComponent,
}

/// Component Identification information
///
/// Identification for a CoreSight component
#[derive(Debug, PartialEq)]
pub struct CSComponentId {
    base_address: u64,
    class: CSComponentClass,
    pub peripheral_id: PeripheralID,
}

/// A reader to extract infromation from a CoreSight component table.
pub struct ComponentInformationReader<'p, P: MI> {
    base_address: u64,
    probe: &'p RefCell<P>,
}

impl<'p, P: MI> ComponentInformationReader<'p, P> {
    /// Creates a new `ComponentInformationReader`.
    pub fn new(base_address: u64, probe: &'p RefCell<P>) -> Self {
        ComponentInformationReader {
            base_address,
            probe,
        }
    }

    /// Reads the component class from a component info table.
    pub fn component_class(&mut self) -> Result<CSComponentClass, RomTableError> {
        #![allow(clippy::verbose_bit_mask)]
        let mut cidr = [0u32; 4];
        let mut probe = self.probe.borrow_mut();

        probe.read_block32(self.base_address as u32 + 0xFF0, &mut cidr)?;

        debug!("CIDR: {:x?}", cidr);

        let preambles = [
            cidr[0] & 0xff,
            cidr[1] & 0x0f,
            cidr[2] & 0xff,
            cidr[3] & 0xff,
        ];

        let expected = [0x0D, 0x0, 0x05, 0xB1];

        for i in 0..4 {
            if preambles[i] != expected[i] {
                warn!(
                    "Component at 0x{:x}: CIDR{} has invalid preamble (expected 0x{:x}, got 0x{:x})",
                    self.base_address, i, expected[i], preambles[i],
                );
                return Err(RomTableError::CSComponentIdentificationError);
            }
        }

        FromPrimitive::from_u32((cidr[1] >> 4) & 0x0F)
            .ok_or(RomTableError::CSComponentIdentificationError)
    }

    /// Reads the peripheral ID from a component info table.
    pub fn peripheral_id(&mut self) -> Result<PeripheralID, RomTableError> {
        let mut probe = self.probe.borrow_mut();

        let mut data = [0u32; 8];

        let peripheral_id_address = self.base_address + 0xFD0;

        debug!(
            "Reading debug id from address: {:08x}",
            peripheral_id_address
        );

        probe.read_block32(self.base_address as u32 + 0xFD0, &mut data[4..])?;
        probe.read_block32(self.base_address as u32 + 0xFE0, &mut data[..4])?;

        debug!("Raw peripheral id: {:x?}", data);

        Ok(PeripheralID::from_raw(&data))
    }

    /// Reads all component properties from a component info table
    pub fn read_all(&mut self) -> Result<CSComponentId, RomTableError> {
        Ok(CSComponentId {
            base_address: self.base_address,
            class: self.component_class()?,
            peripheral_id: self.peripheral_id()?,
        })
    }
}

pub struct CSComponentIter<'a> {
    component: Option<&'a CSComponent>,
    // Signalized whether we are working on the inner iterator already or still need to return self.
    inner: Option<usize>,
}

impl<'a> Iterator for CSComponentIter<'a> {
    type Item = &'a CSComponent;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(i) = self.inner {
            let ret = match self.component.expect("This is a bug. Please report it.") {
                CSComponent::Class1RomTable(_, v) => v.entries.get(i).map(|v| &v.component_data),
                _ => None,
            };
            if ret.is_some() {
                self.inner = Some(i + 1);
            } else {
                self.inner = None;
            }
            ret
        } else {
            let ret = self.component;
            self.component = None;
            ret
        }
    }
}

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Primitive, Debug, PartialEq)]
pub enum CSComponentClass {
    GenericVerificationComponent = 0,
    RomTable = 1,
    CoreSightComponent = 9,
    PeripheralTestBlock = 0xB,
    GenericIPComponent = 0xE,
    CoreLinkOrPrimeCellOrSystemComponent = 0xF,
}

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Debug, PartialEq)]
pub enum CSComponent {
    GenericVerificationComponent(CSComponentId),
    Class1RomTable(CSComponentId, RomTable),
    Class9RomTable(CSComponentId),
    PeripheralTestBlock(CSComponentId),
    GenericIPComponent(CSComponentId),
    CoreLinkOrPrimeCellOrSystemComponent(CSComponentId),
    None,
}

impl CSComponent {
    /// Tries to parse a CoreSight component table.
    pub fn try_parse<P>(link: &RefCell<P>, baseaddr: u64) -> Result<CSComponent, RomTableError>
    where
        P: MI + APAccess<GenericAP, IDR> + APAccess<MemoryAP, BASE> + APAccess<MemoryAP, BASE2>,
    {
        info!("\tReading component data at: {:08x}", baseaddr);

        let component_id = ComponentInformationReader::new(baseaddr, link).read_all()?;

        // Determine the component class to find out what component we are dealing with.
        info!("\tComponent class: {:x?}", component_id.class);

        // Determine the peripheral id to find out what peripheral we are dealing with.
        info!(
            "\tComponent peripheral id: {:x?}",
            component_id.peripheral_id
        );

        let class = match component_id.class {
            CSComponentClass::GenericVerificationComponent => {
                CSComponent::GenericVerificationComponent(component_id)
            }
            CSComponentClass::RomTable => {
                let rom_table = RomTable::try_parse(link, component_id.base_address);

                CSComponent::Class1RomTable(component_id, rom_table)
            }
            CSComponentClass::CoreSightComponent => CSComponent::Class9RomTable(component_id),
            CSComponentClass::PeripheralTestBlock => CSComponent::PeripheralTestBlock(component_id),
            CSComponentClass::GenericIPComponent => CSComponent::GenericIPComponent(component_id),
            CSComponentClass::CoreLinkOrPrimeCellOrSystemComponent => {
                CSComponent::CoreLinkOrPrimeCellOrSystemComponent(component_id)
            }
        };

        Ok(class)
    }

    pub fn iter(&self) -> CSComponentIter {
        CSComponentIter {
            component: Some(self),
            inner: Some(0),
        }
    }
}

/// Indicates component modifications by the implementor of a CoreSight component.
#[derive(Debug, PartialEq)]
pub enum ComponentModification {
    /// Indicates that no specific modification was made.
    No,
    /// Indicates that a modification was made and which one with the contained number.
    Yes(u8),
}

/// Peripheral ID information for a CoreSight component.
///
/// Described in section D1.2.2 of the ADIv5.2 spec.
#[allow(non_snake_case)]
#[derive(Debug, PartialEq)]
pub struct PeripheralID {
    /// Indicates minor errata fixes by the component `designer`.
    pub REVAND: u8,
    /// Indicates component modifications by the `implementor`.
    pub CMOD: ComponentModification,
    /// Indicates major component revisions by the component `designer`.
    pub REVISION: u8,
    /// Indicates the component `designer`.
    ///
    /// `None` if it is a legacy component
    pub JEP106: Option<jep106::JEP106Code>,
    /// Indicates the specific component with an ID unique to this component.
    pub PART: u16,
    /// The SIZE is indicated as a multiple of 4k blocks the peripheral occupies.
    pub SIZE: u8,
}

impl PeripheralID {
    /// Extract the peripheral ID of a CoreSight component table.
    fn from_raw(data: &[u32; 8]) -> Self {
        let jep106id = (((data[2] & 0x07) << 4) | ((data[1] >> 4) & 0x0F)) as u8;
        let jep106 = jep106::JEP106Code::new((data[4] & 0x0F) as u8, jep106id);
        let legacy = (data[2] & 0x8) > 1;

        PeripheralID {
            REVAND: ((data[3] >> 4) & 0x0F) as u8,
            CMOD: match (data[3] & 0x0F) as u8 {
                0x0 => ComponentModification::No,
                v => ComponentModification::Yes(v),
            },
            REVISION: ((data[2] >> 4) & 0x0F) as u8,
            JEP106: if legacy { Some(jep106) } else { None },
            PART: (((data[1] & 0x0F) << 8) | (data[0] & 0xFF)) as u16,
            SIZE: 2u32.pow((data[4] >> 4) & 0x0F) as u8,
        }
    }
}
