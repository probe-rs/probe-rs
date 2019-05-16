use log::{
    info,
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
    ap_access::*,
};

pub struct RomTable {

}

pub enum RomTableError {
    Base,
    NotARomtable,
}

impl RomTable {
    pub fn try_parse<P>(link: &mut P, port: u8) -> Result<RomTable, RomTableError>
    where
        P:
            crate::MI
          + APAccess<GenericAP, IDR>
          + APAccess<MemoryAP, BASE>
          + APAccess<MemoryAP, BASE2>
    {
        // First we get the BASE register which lets us extract the BASEADDR required to access the romtable.
        if let Some(baseaddr) = get_base_addr(link, port) {
            // Determine the component class to find out what component we are dealing with.
            let _component_class = get_component_class(link, baseaddr);

            // Determine the peripheral id to find out what peripheral we are dealing with.
            let _peripheral_id = get_peripheral_id(link, baseaddr);
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

/// This enum describes a component.
/// Described in table D1-2 in the ADIv5.2 spec.
#[derive(Primitive, Debug)]
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
fn get_component_class<P>(link: &mut P, baseaddr: u64) -> Option<ComponentClass>
where
    P:
        crate::MI
{
    let mut data = [0u32;4];
    match link.read_block(baseaddr as u32 | 0xFF0, &mut data) {
        Ok(_) => { info!("CIDR contents: {:#x?}", data) },
        Err(e) => { error!("Error reading the CIDR registers: {:?}", e); return None },
    };
    if data[0] & 0xFF == 0x0D
    && data[1] & 0x0F == 0x00
    && data[2] & 0xFF == 0x05
    && data[3] & 0xFF == 0xB1 {
        let component_class = FromPrimitive::from_u32((data[1] >> 4) & 0x0F);
        match &component_class {
            Some(ref cc) => info!("Component class is {:?}", cc),
            None => error!("Component class could not be properly determined."),
        }
        component_class
    } else {
        error!("The CIDR registers did not contain the expected preambles.");
        None
    }
}

#[derive(Debug)]
enum ComponentModification {
    No,
    Yes(u8),
}

#[allow(non_snake_case)]
#[derive(Debug)]
struct PeripheralID {
    pub REVAND: u8,
    pub CMOD: ComponentModification,
    pub REVISION: u8,
    pub JEDEC: bool,
    pub JEP106ID: u8,
    pub JEP106CC: u8,
    pub PART: u16,
    /// The SIZE is indicated as a multiple of 4k blocks the peripheral occupies.
    pub SIZE: u8
}

/// Try retrieve the peripheral id.
/// The CIDR register is described in section D1.2.2 of the ADIv5.2 spec.
fn get_peripheral_id<P>(link: &mut P, baseaddr: u64) -> Option<PeripheralID>
where
    P:
        crate::MI
{
    let mut data = [0u32;8];
    match link.read_block(baseaddr as u32 | 0xFE0, &mut data) {
        Ok(_) => { info!("PIDR contents: {:#x?}", data) },
        Err(e) => { error!("Error reading the PIDR registers: {:?}", e); return None },
    };

    info!("{:#x?}", data[2]);

    let jep106id = (((data[2] & 0x07) << 4) | ((data[1] >> 4) & 0x0F)) as u8;

    let peripheral_id = PeripheralID {
        REVAND: ((data[3] >> 4) & 0x0F) as u8,
        CMOD: match (data[3] & 0x0F) as u8 {
            0x0 => ComponentModification::No,
            v => ComponentModification::Yes(v),
        },
        REVISION: ((data[2] >> 4) & 0x0F) as u8,
        JEDEC: (data[2] & 0x8) > 1,
        JEP106ID: ((1 - jep106id.count_ones() as u8 % 2) << 7) | jep106id,
        JEP106CC: (data[4] & 0x0F) as u8,
        PART: ((data[1] & 0x0F) | (data[0] & 0xFF)) as u16,
        SIZE: 2u32.pow((data[4] >> 4) & 0x0F) as u8
    };

    info!("\n{:#x?}", peripheral_id);
    Some(peripheral_id)
}

/// Retrieves the BASEADDR of a CoreSight component.
/// The layout of the BASE registers is defined in section C2.6.1 of the ADIv5.2 spec.
pub fn get_base_addr<P>(link: &mut P, port: u8) -> Option<u64>
where
    P:
        crate::MI
        + APAccess<GenericAP, IDR>
        + APAccess<MemoryAP, BASE>
        + APAccess<MemoryAP, BASE2>
{
    // First we get the BASE register which lets us extract the BASEADDR required to access the romtable.
    let memory_port = MemoryAP::new(port);
    let base = match link.read_register_ap(memory_port, BASE::default()) {
        Ok(value) => value,
        Err(e) => { error!("Error reading the BASE registers: {:?}", e); return None },
    };
    info!("\n{:#x?}", base);

    let baseaddr = if let BaseaddrFormat::ADIv5 = base.Format {
        let base2 = match link.read_register_ap(memory_port, BASE2::default()) {
            Ok(value) => value,
            Err(e) => { error!("Error reading the BASE registers: {:?}", e); return None }
        };
        info!("\n{:x?}", base2);
        (u64::from(base2.BASEADDR) << 32)
    } else { 0 } | u64::from(base.BASEADDR << 12);

    info!("\nBASEADDR: {:x?}", baseaddr);
    Some(baseaddr)
}