use super::AccessPortError;
use crate::{Core, Error, Memory, MemoryInterface};
use enum_primitive_derive::Primitive;
use num_traits::cast::FromPrimitive;
use thiserror::Error;

/// An error to report any errors that are romtable discovery specific.
#[derive(Error, Debug)]
pub enum RomTableError {
    #[error("Component is not a valid romtable")]
    NotARomtable,
    #[error("An error with the access port occured during runtime")]
    AccessPort(
        #[from]
        #[source]
        AccessPortError,
    ),
    #[error("The CoreSight Component could not be identified")]
    CSComponentIdentification,
    #[error("Could not access romtable")]
    Memory(#[source] Error),
    #[error("The requested component '{0}' was not found")]
    ComponentNotFound(PeripheralType),
    #[error("There are no components to operate on")]
    NoComponents,
}

/// A lazy romtable reader that is used to create an iterator over all romtable entries.
struct RomTableReader<'probe: 'memory, 'memory> {
    base_address: u64,
    memory: &'memory mut Memory<'probe>,
}

/// Iterates over a ROM table non recursively.
impl<'probe: 'memory, 'memory> RomTableReader<'probe, 'memory> {
    fn new(memory: &'memory mut Memory<'probe>, base_address: u64) -> Self {
        RomTableReader {
            base_address,
            memory,
        }
    }

    /// Iterate over all entries of the rom table, non-recursively
    fn entries(&mut self) -> RomTableIterator<'probe, 'memory, '_> {
        RomTableIterator::new(self)
    }
}

/// An iterator to lazily iterate over all the romtable entries in memory.
///
/// For internal use only.
struct RomTableIterator<'probe: 'memory, 'memory: 'reader, 'reader> {
    rom_table_reader: &'reader mut RomTableReader<'probe, 'memory>,
    offset: u64,
}

impl<'probe: 'memory, 'memory: 'reader, 'reader> RomTableIterator<'probe, 'memory, 'reader> {
    /// Creates a new lazy romtable iterator.
    fn new(reader: &'reader mut RomTableReader<'probe, 'memory>) -> Self {
        RomTableIterator {
            rom_table_reader: reader,
            offset: 0,
        }
    }
}

impl<'probe, 'memory, 'reader> Iterator for RomTableIterator<'probe, 'memory, 'reader> {
    type Item = Result<RomTableEntryRaw, RomTableError>;

    fn next(&mut self) -> Option<Self::Item> {
        let component_address = self.rom_table_reader.base_address + self.offset;
        log::info!("Reading rom table entry at {:08x}", component_address);

        self.offset += 4;

        let mut entry_data = [0u32; 1];

        if let Err(e) = self
            .rom_table_reader
            .memory
            .read_32(component_address as u32, &mut entry_data)
        {
            return Some(Err(RomTableError::Memory(e)));
        }

        // End of entries is marked by an all zero entry
        if entry_data[0] == 0 {
            log::info!("Entry consists of all zeroes, stopping.");
            return None;
        }

        let entry_data =
            RomTableEntryRaw::new(self.rom_table_reader.base_address as u32, entry_data[0]);

        log::info!("ROM Table Entry: {:#x?}", entry_data);
        Some(Ok(entry_data))
    }
}

/// Encapsulates information about a CoreSight component.
#[derive(Debug, PartialEq)]
pub struct RomTable {
    /// ALL the entries in the romtable in flattened fashion.
    /// This contains all nested romtable entries.
    entries: Vec<RomTableEntry>,
}

impl RomTable {
    /// Tries to parse a CoreSight component table.
    ///
    /// This does not check whether the data actually signalizes
    /// to contain a ROM table but assumes this was checked beforehand.
    fn try_parse(memory: &mut Memory<'_>, base_address: u64) -> Result<RomTable, RomTableError> {
        let mut entries = vec![];

        log::info!("Parsing romtable at base_address {:x?}", base_address);

        // Read all the raw romtable entries and flatten them.
        let reader = RomTableReader::new(memory, base_address)
            .entries()
            .filter_map(Result::ok)
            .collect::<Vec<RomTableEntryRaw>>();

        // Iterate all entries and get their data.
        for raw_entry in reader.into_iter() {
            let entry_base_addr = raw_entry.component_address();

            log::info!("Parsing entry at {:x?}", entry_base_addr);

            if raw_entry.entry_present {
                let component = Component::try_parse(memory, u64::from(entry_base_addr))?;

                // Finally remmeber the entry.
                entries.push(RomTableEntry {
                    format: raw_entry.format,
                    power_domain_id: raw_entry.power_domain_id,
                    power_domain_valid: raw_entry.power_domain_valid,
                    component,
                });
            }
        }

        Ok(RomTable { entries })
    }
}

/// A ROM table entry with raw information parsed.
///
/// Described in section D3.4.4 of the ADIv5.2 specification.
///
/// This should only be used for parsing the raw memory structures of the entry.
/// Don't use this in the public API.
///
/// For advanced usages, see [RomTableEntry](struct.RomTableEntry.html).
#[derive(Debug, PartialEq)]
struct RomTableEntryRaw {
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
    base_address: u32,
}

impl RomTableEntryRaw {
    /// Create a new RomTableEntryRaw from raw ROM table entry data in memory.
    fn new(base_address: u32, raw: u32) -> Self {
        log::debug!("Parsing raw rom table entry: 0x{:05x}", raw);

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
            base_address,
        }
    }

    /// Returns the address of the CoreSight component behind a ROM table entry.
    pub fn component_address(&self) -> u32 {
        (i64::from(self.base_address) + (i64::from(self.address_offset << 12))) as u32
    }
}

/// A completely finished-parsing romtable entry.
///
/// This struct should be used for public interfacing.
#[derive(Debug, PartialEq)]
struct RomTableEntry {
    /// The power domain ID of the CoreSight component behind the ROM table entry.
    power_domain_id: u8,
    /// The power domain is valid if this is true.
    power_domain_valid: bool,
    /// Reads one if the ROM table has 32bit format.
    ///
    /// It is unsure if it can have a RAZ value.
    format: bool,
    /// The component class of the component pointed to by this romtable entry.
    pub(crate) component: Component,
}

/// Component Identification information
///
/// Identification for a CoreSight component
#[derive(Debug, PartialEq)]
pub struct ComponentId {
    component_address: u64,
    class: RawComponent,
    peripheral_id: PeripheralID,
}

impl ComponentId {
    /// Retreive the address of the component.
    pub fn component_address(&self) -> u64 {
        self.component_address
    }

    /// Retreive the peripheral ID of the component.
    pub fn peripheral_id(&self) -> &PeripheralID {
        &self.peripheral_id
    }
}

/// A reader to extract infromation from a CoreSight component table.
///
/// This reader is meant for internal use only.
pub struct ComponentInformationReader<'probe: 'memory, 'memory> {
    base_address: u64,
    memory: &'memory mut Memory<'probe>,
}

impl<'probe: 'memory, 'memory> ComponentInformationReader<'probe, 'memory> {
    /// Creates a new `ComponentInformationReader` which can be used to extract the data from a component information table in memory.
    pub fn new(base_address: u64, memory: &'memory mut Memory<'probe>) -> Self {
        ComponentInformationReader {
            base_address,
            memory,
        }
    }

    /// Reads the component class from a component information table.
    ///
    /// This function does a direct memory access and is meant for internal use only.
    fn component_class(&mut self) -> Result<RawComponent, RomTableError> {
        #![allow(clippy::verbose_bit_mask)]
        let mut cidr = [0u32; 4];

        self.memory
            .read_32(self.base_address as u32 + 0xFF0, &mut cidr)
            .map_err(RomTableError::Memory)?;

        log::debug!("CIDR: {:x?}", cidr);

        let preambles = [
            cidr[0] & 0xff,
            cidr[1] & 0x0f,
            cidr[2] & 0xff,
            cidr[3] & 0xff,
        ];

        let expected = [0x0D, 0x0, 0x05, 0xB1];

        for i in 0..4 {
            if preambles[i] != expected[i] {
                log::warn!(
                    "Component at 0x{:x}: CIDR{} has invalid preamble (expected 0x{:x}, got 0x{:x})",
                    self.base_address, i, expected[i], preambles[i],
                );
                // Technically invalid preambles are a no-go.
                // We are not sure if we need to abort earlier or if just emitting a warning is okay.
                // For now this works, so we emit a warning and continue on.
            }
        }

        FromPrimitive::from_u32((cidr[1] >> 4) & 0x0F)
            .ok_or(RomTableError::CSComponentIdentification)
    }

    /// Reads the peripheral ID from a component information table.
    ///
    /// This function does a direct memory access and is meant for internal use only.
    fn peripheral_id(&mut self) -> Result<PeripheralID, RomTableError> {
        let mut data = [0u32; 8];

        let peripheral_id_address = self.base_address + 0xFD0;

        log::debug!(
            "Reading debug id from address: {:08x}",
            peripheral_id_address
        );

        self.memory
            .read_32(self.base_address as u32 + 0xFD0, &mut data[4..])
            .map_err(RomTableError::Memory)?;
        self.memory
            .read_32(self.base_address as u32 + 0xFE0, &mut data[..4])
            .map_err(RomTableError::Memory)?;

        log::debug!("Raw peripheral id: {:x?}", data);

        const DEV_TYPE_OFFSET: u32 = 0xFCC;
        const DEV_TYPE_MASK: u32 = 0xFF;

        let dev_type = self
            .memory
            .read_word_32(self.base_address as u32 + DEV_TYPE_OFFSET)
            .map(|v| (v & DEV_TYPE_MASK) as u8)
            .map_err(|e| RomTableError::Memory(e))?;

        const ARCH_ID_OFFSET: u32 = 0xFBC;
        const ARCH_ID_MASK: u32 = 0xFFFF;
        const ARCH_ID_PRESENT_BIT: u32 = 1 << 20;

        let arch_id = self
            .memory
            .read_word_32(self.base_address as u32 + ARCH_ID_OFFSET)
            .map(|v| {
                if v & ARCH_ID_PRESENT_BIT > 0 {
                    (v & ARCH_ID_MASK) as u16
                } else {
                    0
                }
            })
            .map_err(|e| RomTableError::Memory(e))?;

        log::debug!("Dev type: {:x}, arch id: {:x}", dev_type, arch_id);

        Ok(PeripheralID::from_raw(&data, dev_type, arch_id))
    }

    /// Reads all component properties from a component info table
    ///
    /// This function does a direct memory access and is meant for internal use only.
    fn read_all(&mut self) -> Result<ComponentId, RomTableError> {
        Ok(ComponentId {
            component_address: self.base_address,
            class: self.component_class()?,
            peripheral_id: self.peripheral_id()?,
        })
    }
}

/// This enum describes the class of a CoreSight component.
///
/// This does not describe the exact component type which is determined via the `PeripheralID`.
///
/// Meant for internal parsing usage only.
///
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Primitive, Debug, PartialEq)]
enum RawComponent {
    GenericVerificationComponent = 0,
    RomTable = 1,
    CoreSightComponent = 9,
    PeripheralTestBlock = 0xB,
    GenericIPComponent = 0xE,
    CoreLinkOrPrimeCellOrSystemComponent = 0xF,
}

/// This enum describes a CoreSight component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Debug, PartialEq)]
pub enum Component {
    GenericVerificationComponent(ComponentId),
    Class1RomTable(ComponentId, RomTable),
    Class9RomTable(ComponentId),
    PeripheralTestBlock(ComponentId),
    GenericIPComponent(ComponentId),
    CoreLinkOrPrimeCellOrSystemComponent(ComponentId),
}

impl Component {
    /// Tries to parse a CoreSight component table.
    pub fn try_parse<'probe: 'memory, 'memory>(
        memory: &'memory mut Memory<'probe>,
        baseaddr: u64,
    ) -> Result<Component, RomTableError> {
        log::info!("\tReading component data at: {:08x}", baseaddr);

        let component_id = ComponentInformationReader::new(baseaddr, memory).read_all()?;

        // Determine the component class to find out what component we are dealing with.
        log::info!("\tComponent class: {:x?}", component_id.class);

        // Determine the peripheral id to find out what peripheral we are dealing with.
        log::info!(
            "\tComponent peripheral id: {:x?}",
            component_id.peripheral_id
        );

        if let Some(info) = component_id.peripheral_id.determine_part() {
            log::info!("\tComponent is known: {}", info);
        }

        let class = match component_id.class {
            RawComponent::GenericVerificationComponent => {
                Component::GenericVerificationComponent(component_id)
            }
            RawComponent::RomTable => {
                let rom_table = RomTable::try_parse(memory, component_id.component_address)?;

                Component::Class1RomTable(component_id, rom_table)
            }
            RawComponent::CoreSightComponent => Component::Class9RomTable(component_id),
            RawComponent::PeripheralTestBlock => Component::PeripheralTestBlock(component_id),
            RawComponent::GenericIPComponent => Component::GenericIPComponent(component_id),
            RawComponent::CoreLinkOrPrimeCellOrSystemComponent => {
                Component::CoreLinkOrPrimeCellOrSystemComponent(component_id)
            }
        };

        Ok(class)
    }

    pub fn id(&self) -> &ComponentId {
        match self {
            Component::GenericVerificationComponent(component_id) => component_id,
            Component::Class1RomTable(component_id, ..) => component_id,
            Component::Class9RomTable(component_id) => component_id,
            Component::PeripheralTestBlock(component_id) => component_id,
            Component::GenericIPComponent(component_id) => component_id,
            Component::CoreLinkOrPrimeCellOrSystemComponent(component_id) => component_id,
        }
    }

    /// Reads a register of the component pointed to by this romtable entry.
    pub fn read_reg(&self, core: &mut Core, offset: u32) -> Result<u32, Error> {
        let value = core.read_word_32(self.id().component_address as u32 + offset)?;
        Ok(value)
    }

    /// Writes a register of the component pointed to by this romtable entry.
    pub fn write_reg(&self, core: &mut Core, offset: u32, value: u32) -> Result<(), Error> {
        core.write_word_32(self.id().component_address as u32 + offset, value)?;
        Ok(())
    }

    pub fn find_component<'core>(
        &'core self,
        peripheral_type: PeripheralType,
    ) -> Option<&'core Component> {
        for component in self.iter() {
            if component.id().peripheral_id.is_of_type(peripheral_type) {
                return Some(component);
            }
        }
        None
    }

    pub fn iter(&self) -> ComponentIter {
        ComponentIter::new(vec![self])
    }
}

/// This is a recursive iterator over all CoreSight components.
pub struct ComponentIter<'a> {
    /// The components of this iterator level.
    components: Vec<&'a Component>,
    /// The index of the item of the current level that should be returned next.
    current: usize,
    /// A possible child iterator. Always iterated first if there is a non exhausted one present.
    children: Option<Box<ComponentIter<'a>>>,
}

impl<'a> ComponentIter<'a> {
    pub fn new(components: Vec<&'a Component>) -> Self {
        Self {
            components,
            current: 0,
            children: None,
        }
    }
}

impl<'a> Iterator for ComponentIter<'a> {
    type Item = &'a Component;

    fn next(&mut self) -> Option<Self::Item> {
        // If we have children to iterate, do that first.
        if let Some(children) = &mut self.children {
            // If the iterator is not yet exhausted, return the next item.
            if let Some(child) = children.next() {
                return Some(child);
            } else {
                // Else just return to iterating ourselves.
                self.children = None;
            }
        }

        // If we have one more component to iterate, just return that first (do some other stuff first tho!).
        if let Some(component) = self.components.get(self.current) {
            // If it has children, remember to iterate them next.
            self.children = match component {
                Component::Class1RomTable(_, v) => Some(Box::new(ComponentIter::new(
                    v.entries.iter().map(|v| &v.component).collect(),
                ))),
                _ => None,
            };
            // Advance the pointer by one.
            self.current += 1;
            return Some(component);
        }

        // If we get until here, we have no more children and no more of our own items to iterate,
        // so we just always return None.

        None
    }
}

/// Indicates component modifications by the implementor of a CoreSight component.
#[derive(Debug, PartialEq)]
enum ComponentModification {
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
    REVAND: u8,
    /// Indicates component modifications by the `implementor`.
    CMOD: ComponentModification,
    /// Indicates major component revisions by the component `designer`.
    REVISION: u8,
    /// Indicates the component `designer`.
    ///
    /// `None` if it is a legacy component
    JEP106: Option<jep106::JEP106Code>,
    /// Indicates the specific component with an ID unique to this component.
    PART: u16,
    /// The SIZE is indicated as a multiple of 4k blocks the peripheral occupies.
    SIZE: u8,

    dev_type: u8,
    arch_id: u16,
}

impl PeripheralID {
    const _ITM_PID: [u8; 8] = [0x1, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];
    const _TPIU_PID: [u8; 8] = [0xA1, 0xB9, 0x0B, 0x0, 0x4, 0x0, 0x0, 0x0];
    const _DWT_PID: [u8; 8] = [0x2, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];

    /// Extracts the peripheral ID of the CoreSight component table data.
    fn from_raw(data: &[u32; 8], dev_type: u8, arch_id: u16) -> Self {
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
            dev_type,
            arch_id,
        }
    }

    /// Returns whether the peripheral is of the given type.
    pub fn is_of_type(&self, peripheral_type: PeripheralType) -> bool {
        self.determine_part()
            .map(|info| info.peripheral_type() == peripheral_type)
            .unwrap_or(false)
    }

    /// Returns the JEP106 code of the peripheral ID register.
    pub fn jep106(&self) -> Option<jep106::JEP106Code> {
        self.JEP106
    }

    /// Returns the PART of the peripheral ID register.
    pub fn part(&self) -> u16 {
        self.PART
    }

    #[rustfmt::skip]
    pub fn determine_part(&self) -> Option<ComponentInfo> {
        let code = self.JEP106.map(|jep106| jep106.get()).flatten().unwrap_or("");

        // Source of the table: https://github.com/blacksphere/blackmagic/blob/master/src/target/adiv5.c#L189
        // Not all are present and this table could be expanded
        let arm_part = match (
            code,
            self.PART,
            self.dev_type,
            self.arch_id,
        ) {
            ("ARM Ltd", 0x000, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M3 SCS", PeripheralType::Scs)),
            ("ARM Ltd", 0x001, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M3 ITM", PeripheralType::Itm)),
            ("ARM Ltd", 0x002, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M3 DWT", PeripheralType::Dwt)),
            ("ARM Ltd", 0x003, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M3 FBP", PeripheralType::Fbp)),
            ("ARM Ltd", 0x008, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M0 SCS", PeripheralType::Scs)),
            ("ARM Ltd", 0x00A, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M0 DWT", PeripheralType::Dwt)),
            ("ARM Ltd", 0x00B, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M0 BPU", PeripheralType::Bpu)),
            ("ARM Ltd", 0x00C, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M4 SCS", PeripheralType::Scs)),
            ("ARM Ltd", 0x00D, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight ETM11", PeripheralType::Etm)),
            ("ARM Ltd", 0x00E, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M7 FBP", PeripheralType::Fbp)),
            ("ARM Ltd", 0x101, 0x00, 0x0000) => Some(ComponentInfo::new("System TSGEN", PeripheralType::Tsgen)),
            ("ARM Ltd", 0x471, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M0  ROM", PeripheralType::Rom)),
            ("ARM Ltd", 0x4C0, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M0+ ROM", PeripheralType::Rom)),
            ("ARM Ltd", 0x4C4, 0x00, 0x0000) => Some(ComponentInfo::new("Cortex-M4 ROM", PeripheralType::Rom)),
            ("ARM Ltd", 0x907, 0x21, 0x0000) => Some(ComponentInfo::new("CoreSight ETB", PeripheralType::Etb)),
            ("ARM Ltd", 0x910, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight ETM9", PeripheralType::Etm)),
            ("ARM Ltd", 0x912, 0x11, 0x0000) => Some(ComponentInfo::new("CoreSight TPIU", PeripheralType::Tpiu)),
            ("ARM Ltd", 0x913, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight ITM", PeripheralType::Itm)),
            ("ARM Ltd", 0x914, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight SWO", PeripheralType::Swo)),
            ("ARM Ltd", 0x920, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight ETM11", PeripheralType::Etm)),
            ("ARM Ltd", 0x923, 0x11, 0x0000) => Some(ComponentInfo::new("Cortex-M3 TPIU", PeripheralType::Tpiu)),
            ("ARM Ltd", 0x924, 0x13, 0x0000) => Some(ComponentInfo::new("Cortex-M3 ETM", PeripheralType::Etm)),
            ("ARM Ltd", 0x925, 0x13, 0x0000) => Some(ComponentInfo::new("Cortex-M4 ETM", PeripheralType::Etm)),
            ("ARM Ltd", 0x962, 0x00, 0x0000) => Some(ComponentInfo::new("CoreSight STM", PeripheralType::Stm)),
            ("ARM Ltd", 0x963, 0x63, 0x0a63) => Some(ComponentInfo::new("CoreSight STM", PeripheralType::Stm)),
            ("ARM Ltd", 0x975, 0x13, 0x4a13) => Some(ComponentInfo::new("Cortex-M7 ETM", PeripheralType::Etm)),
            ("ARM Ltd", 0x9A1, 0x11, 0x0000) => Some(ComponentInfo::new("Cortex-M4 TPIU", PeripheralType::Tpiu)),
            ("ARM Ltd", 0x9A9, 0x11, 0x0000) => Some(ComponentInfo::new("Cortex-M7 TPIU", PeripheralType::Tpiu)),
            ("ARM Ltd", 0xD20, 0x00, 0x2A04) => Some(ComponentInfo::new("Cortex-M23 SCS", PeripheralType::Scs)),
            ("ARM Ltd", 0xD20, 0x11, 0x0000) => Some(ComponentInfo::new("Cortex-M23 TPIU", PeripheralType::Tpiu)),
            ("ARM Ltd", 0xD20, 0x13, 0x0000) => Some(ComponentInfo::new("Cortex-M23 ETM", PeripheralType::Etm)),
            ("ARM Ltd", 0xD20, 0x00, 0x1A02) => Some(ComponentInfo::new("Cortex-M23 DWT", PeripheralType::Dwt)),
            ("ARM Ltd", 0xD20, 0x00, 0x1A03) => Some(ComponentInfo::new("Cortex-M23 BPU", PeripheralType::Bpu)),
            ("ARM Ltd", 0xD21, 0x00, 0x2A04) => Some(ComponentInfo::new("Cortex-M33 SCS", PeripheralType::Scs)),
            ("ARM Ltd", 0xD21, 0x43, 0x1A01) => Some(ComponentInfo::new("Cortex-M33 ITM", PeripheralType::Itm)),
            ("ARM Ltd", 0xD21, 0x00, 0x1A02) => Some(ComponentInfo::new("Cortex-M33 DWT", PeripheralType::Dwt)),
            ("ARM Ltd", 0xD21, 0x00, 0x1A03) => Some(ComponentInfo::new("Cortex-M33 BPU", PeripheralType::Bpu)),
            ("ARM Ltd", 0xD21, 0x13, 0x4A13) => Some(ComponentInfo::new("Cortex-M33 ETM", PeripheralType::Etm)),
            ("ARM Ltd", 0xD21, 0x11, 0x0000) => Some(ComponentInfo::new("Cortex-M33 TPIU", PeripheralType::Tpiu)),
            _ => None,
        };

        if arm_part.is_some() {
            return arm_part;
        }

        let stm_part = if code == "STMicroelectronics" && self.dev_type <= 0x01 && self.arch_id == 0x00 {
            Stm32ID::from_u16(self.PART).map(|id| ComponentInfo::new("STM device id", PeripheralType::Stm32ID(id)))
        } else {
            None
        };

        if stm_part.is_some() {
            return stm_part;
        }

        None
    }
}

#[derive(Debug, Copy, Clone)]
pub struct ComponentInfo {
    name: &'static str,
    peripheral_type: PeripheralType,
}

impl ComponentInfo {
    pub const fn new(name: &'static str, peripheral_type: PeripheralType) -> Self {
        Self {
            name,
            peripheral_type,
        }
    }

    pub const fn name(&self) -> &'static str {
        self.name
    }

    pub const fn peripheral_type(&self) -> PeripheralType {
        self.peripheral_type
    }
}

impl std::fmt::Display for ComponentInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name, self.peripheral_type)
    }
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum PeripheralType {
    Tpiu,
    Itm,
    Dwt,
    Scs,
    Fbp,
    Bpu,
    Etm,
    Etb,
    Rom,
    Swo,
    Stm,
    Tsgen,
    Stm32ID(Stm32ID),
}

impl std::fmt::Display for PeripheralType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeripheralType::Tpiu => write!(f, "Tpiu (Trace Port Interface Unit)"),
            PeripheralType::Itm => write!(f, "Itm (Instrumentation Trace Module)"),
            PeripheralType::Dwt => write!(f, "Dwt (Data Watchpoint and Trace)"),
            PeripheralType::Scs => write!(f, "Scs (System Control Space)"),
            PeripheralType::Fbp => write!(f, "Fbp (Flash Patch and Breakpoint)"),
            PeripheralType::Bpu => write!(f, "Bpu (Breakpoint Unit)"),
            PeripheralType::Etm => write!(f, "Etm (Embedded Trace)"),
            PeripheralType::Etb => write!(f, "Etb (Trace Buffer)"),
            PeripheralType::Rom => write!(f, "Rom"),
            PeripheralType::Swo => write!(f, "Swo (Single Wire Output)"),
            PeripheralType::Stm => write!(f, "Stm (System Trace Macrocell)"),
            PeripheralType::Tsgen => write!(f, "Tsgen (Time Stamp Generator)"),
            PeripheralType::Stm32ID(id) => write!(f, "{:?}", id),
        }
    }
}

#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, Primitive)]
pub enum Stm32ID {
    STM32H74X = 0x450,
    STM32H7BX = 0x480,
    STM32H72X = 0x483,

    STM32L41 = 0x464,
    STM32L43 = 0x435,
    STM32L45 = 0x462,
    STM32L47 = 0x415,
    STM32L49 = 0x461,
    STM32L4R = 0x470,

    STM32G03 = 0x466,
    STM32G07 = 0x460,

    STM32G43 = 0x468,
    STM32G47 = 0x469,

    STM32F20X = 0x411,

    STM32F40X = 0x413,
    STM32F42X = 0x419,
    STM32F446 = 0x421,
    STM32F401C = 0x423,
    STM32F411 = 0x431,
    STM32F401E = 0x433,
    STM32F46X = 0x434,
    STM32F412 = 0x441,
    STM32F410 = 0x458,
    STM32F413 = 0x463,

    STM32F74X = 0x449,
    STM32F76X = 0x451,
    STM32F72X = 0x452,

    STM32F1LD = 0x412, // Low density
    STM32F1MD = 0x410, // Medium density
    STM32F1HD = 0x414, // High density
    STM32F1XL = 0x430, // XL density
    STM32F1CD = 0x418, // Connectivity device

    STM32F328 = 0x438, // STM32F303x6/8 and STM32F328
    STM32F30X = 0x422,
    STM32F398XE = 0x446, // STM32F303xD/E and STM32F398xE
    STM32F37X = 0x432,
    STM32F302C8 = 0x439,

    STM32F03 = 0x444,
    STM32F04 = 0x445, // STM32F04/F070x6
    STM32F05 = 0x440, // STM32F05/F030x8
    STM32F07 = 0x448,
    STM32F09 = 0x442, // STM32F09/F030xC

    STM32L0XC1 = 0x457, // STM32L0xx Cat1
    STM32L0XC2 = 0x425, // STM32L0xx Cat2
    STM32L0XC3 = 0x417, // STM32L0xx Cat3
    STM32L0XC5 = 0x447, // STM32L0xx Cat5

    STM32L1XC1 = 0x416, // STM32L1xx Cat1
    STM32L1XC2 = 0x429, // STM32L1xx Cat2
    STM32L1XC3 = 0x427, // STM32L1xx Cat3
    STM32L1XC4 = 0x436, // STM32L1xx Cat4
    STM32L1XC5 = 0x437, // STM32L1xx Cat5

    STM32L5 = 0x472, // STM32L552xx and STM32L562x
}

impl Stm32ID {
    pub fn is_f0_series(&self) -> bool {
        match self {
            Stm32ID::STM32F03 => true,
            Stm32ID::STM32F04 => true,
            Stm32ID::STM32F05 => true,
            Stm32ID::STM32F07 => true,
            Stm32ID::STM32F09 => true,
            _ => false,
        }
    }

    pub fn is_f1_series(&self) -> bool {
        match self {
            Stm32ID::STM32F1LD => true,
            Stm32ID::STM32F1MD => true,
            Stm32ID::STM32F1HD => true,
            Stm32ID::STM32F1XL => true,
            Stm32ID::STM32F1CD => true,
            _ => false,
        }
    }

    pub fn is_f2_series(&self) -> bool {
        match self {
            Stm32ID::STM32F20X => true,
            _ => false,
        }
    }

    pub fn is_f3_series(&self) -> bool {
        match self {
            Stm32ID::STM32F328 => true,
            Stm32ID::STM32F30X => true,
            Stm32ID::STM32F398XE => true,
            Stm32ID::STM32F37X => true,
            Stm32ID::STM32F302C8 => true,
            _ => false,
        }
    }

    pub fn is_f4_series(&self) -> bool {
        match self {
            Stm32ID::STM32F40X => true,
            Stm32ID::STM32F42X => true,
            Stm32ID::STM32F446 => true,
            Stm32ID::STM32F401C => true,
            Stm32ID::STM32F411 => true,
            Stm32ID::STM32F401E => true,
            Stm32ID::STM32F46X => true,
            Stm32ID::STM32F412 => true,
            Stm32ID::STM32F410 => true,
            Stm32ID::STM32F413 => true,
            _ => false,
        }
    }

    pub fn is_f7_series(&self) -> bool {
        match self {
            Stm32ID::STM32F74X => true,
            Stm32ID::STM32F76X => true,
            Stm32ID::STM32F72X => true,
            _ => false,
        }
    }

    pub fn is_l0_series(&self) -> bool {
        match self {
            Stm32ID::STM32L0XC1 => true,
            Stm32ID::STM32L0XC2 => true,
            Stm32ID::STM32L0XC3 => true,
            Stm32ID::STM32L0XC5 => true,
            _ => false,
        }
    }

    pub fn is_l1_series(&self) -> bool {
        match self {
            Stm32ID::STM32L1XC1 => true,
            Stm32ID::STM32L1XC2 => true,
            Stm32ID::STM32L1XC3 => true,
            Stm32ID::STM32L1XC4 => true,
            Stm32ID::STM32L1XC5 => true,
            _ => false,
        }
    }

    pub fn is_l4_series(&self) -> bool {
        match self {
            Stm32ID::STM32L41 => true,
            Stm32ID::STM32L43 => true,
            Stm32ID::STM32L45 => true,
            Stm32ID::STM32L47 => true,
            Stm32ID::STM32L49 => true,
            Stm32ID::STM32L4R => true,
            _ => false,
        }
    }

    pub fn is_l5_series(&self) -> bool {
        match self {
            Stm32ID::STM32L5 => true,
            _ => false,
        }
    }

    pub fn is_g0_series(&self) -> bool {
        match self {
            Stm32ID::STM32G03 => true,
            Stm32ID::STM32G07 => true,
            _ => false,
        }
    }

    pub fn is_g4_series(&self) -> bool {
        match self {
            Stm32ID::STM32G43 => true,
            Stm32ID::STM32G47 => true,
            _ => false,
        }
    }

    pub fn is_h7_series(&self) -> bool {
        match self {
            Stm32ID::STM32H74X => true,
            Stm32ID::STM32H7BX => true,
            Stm32ID::STM32H72X => true,
            _ => false,
        }
    }
}
