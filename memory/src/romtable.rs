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
    pub fn entries(&mut self) -> Result<Vec<RomTableEntry>, RomTableError> {
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

            let entry_data = RomTableEntry::new(self.base_address as u32, entry_data[0]);


            info!("ROM Table Entry: {:x?}", entry_data);

            entries.push(entry_data);
        }

        Ok(entries)
    }
}

#[derive(Debug)]
pub struct RomTable {

}

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

impl RomTable {
    pub fn try_parse<P>(link: &mut P, baseaddr: u64) -> Result<RomTable, RomTableError>
    where
        P:
            crate::MI
          + APAccess<GenericAP, IDR>
          + APAccess<MemoryAP, BASE>
          + APAccess<MemoryAP, BASE2>
    {
        // Determine the component class to find out what component we are dealing with.
        let component_class = get_component_class(link, baseaddr);

        // Determine the peripheral id to find out what peripheral we are dealing with.
        let peripheral_id = get_peripheral_id(link, baseaddr);

        info!("Peripheral id: {:x?}", peripheral_id);

        if Ok(ComponentClass::RomTable) != component_class {
            return Err(RomTableError::NotARomtable);
        }


        for component_offset in (0..0xfcc).step_by(4) {
            info!("Reading rom table entry at {:08x}", baseaddr + component_offset);

            let mut entry_data = [0u32;1];
            link.read_block((baseaddr + component_offset) as u32, &mut entry_data)?;

            // end of entries is marked by an all zero entry
            if entry_data[0] == 0 {
                info!("Entry consists of all zeroes, stopping.");
                break;
            }

            let entry_data = RomTableEntry::new(baseaddr as u32, entry_data[0]);

            info!("ROM Table Entry: {:x?}", entry_data);

            let entry_base_addr = entry_data.component_addr();
            info!("\tEntry address: {:08x}", entry_base_addr);

            let component_peripheral_id = get_peripheral_id(link, entry_base_addr as u64)?;

            info!("\tComponent peripheral id: {:x?}", component_peripheral_id);


            let component_class = get_component_class(link, entry_base_addr as u64)?;

            info!("\tComponent class: {:x?}", component_class);

            let scs_peripheral_id = PeripheralID::from_raw(&[
                0x8,
                0xb0,
                0xb,
                0x0,
                0x4,
                0x0,
                0x0,
                0x0,
            ]);

            let scs_component_class = ComponentClass::GenericIPComponent;

            if component_peripheral_id == scs_peripheral_id && component_class == scs_component_class {
                // found the scs (System Control Space) register of a cortex m0
                println!("Found SCS CoreSight at address 0x{:08x}", entry_base_addr);
                
                // this means we can check the cpuid register (located at 0xe000_ed00)
                let cpu_id: u32 = link.read(0xe000_ed00)?;

                println!("CPUID: 0x{:08X}", cpu_id);
            }

            if component_class == ComponentClass::RomTable {
                info!("Recursively parsing ROM table at address 0x{:08x}", entry_base_addr);
                let _table = RomTable::try_parse(link, entry_base_addr as u64);
                info!("Finished parsing entries.");
            }
        }


        // CoreSight identification register offsets.
        //const DEVARCH: u32 = 0xfbc;
        // const DEVID: u32 = 0xfc8;
        // const DEVTYPE: u32 = 0xfcc;
        // const PIDR4: u32 = 0xfd0;
        // const PIDR0: u32 = 0xfe0;
        //const CIDR0: u32 = 0xff0;
        // const IDR_END: u32 = 0x1000;

        // Range of identification registers to read at once and offsets in results.
        //
        // To improve component identification performance, we read all of a components
        // CoreSight ID registers in a single read. Reading starts at the DEVARCH register.
        //const IDR_READ_START: u32 = DEVARCH;
        // const IDR_READ_COUNT: u32 = (IDR_END - IDR_READ_START) / 4;
        // const DEVARCH_OFFSET: u32 = (DEVARCH - IDR_READ_START) / 4;
        // const DEVTYPE_OFFSET: u32 = (DEVTYPE - IDR_READ_START) / 4;
        // const PIDR4_OFFSET: u32 = (PIDR4 - IDR_READ_START) / 4;
        // const PIDR0_OFFSET: u32 = (PIDR0 - IDR_READ_START) / 4;
        //const CIDR0_OFFSET: u32 = (CIDR0 - IDR_READ_START) / 4;

        //let cidr = extract_id_register_value(data.as_slice(), CIDR0_OFFSET);
        //println!("{:08X?}", cidr);

        Ok(RomTable {})
    }
}

#[derive(Debug)]
pub struct RomTableEntry {
    address_offset: i32,
    power_domain_id: u8,
    power_domain_valid: bool,
    format: bool,
    entry_present: bool,
    // Base address of the rom table
    base_addr: u32,         
}

impl  RomTableEntry {
    fn new(base_addr: u32, raw: u32) -> Self {
        debug!("Parsing raw rom table entry: 0x{:05x}", raw);

        let address_offset = ((raw >> 12) & 0xf_ff_ff) as i32;
        let power_domain_id = ((raw >> 4) & 0xf) as u8;
        let power_domain_valid = (raw & 4) == 4;
        let format = (raw & 2) == 2;
        let entry_present = (raw & 1) == 1;

        RomTableEntry {
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
    pub JEP106: &'static str,
    pub PART: u16,
    /// The SIZE is indicated as a multiple of 4k blocks the peripheral occupies.
    pub SIZE: u8
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
            JEP106: jep106::get((data[4] & 0x0F) as u8, ((1 - jep106id.count_ones() as u8 % 2) << 7) | jep106id),
            PART: ((data[1] & 0x0F) | (data[0] & 0xFF)) as u16,
            SIZE: 2u32.pow((data[4] >> 4) & 0x0F) as u8
        }
    }
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