use crate::common::{
    CliError,
    with_device,
};

use ocd::{
    coresight::{
        access_ports::{
            generic_ap::{
                GenericAP,
                IDR,
                APClass,
            },
            memory_ap::{
                MemoryAP,
                BASE,
                BASE2,
                BaseaddrFormat,
            },
        },
        ap_access::{
            APAccess,
            access_port_is_valid,
        },
    },
    memory::romtable::CSComponent,
    target::Target,
};

pub fn show_info_of_device(n: usize, target: Target) -> Result<(), CliError> {
    with_device(n, target, |mut session| {

        /*
            The following code only works with debug port v2,
            which might not necessarily be present.

            Once the typed interface for the debug port is done, it 
            can be enabled again.

        println!("Device information:");


        let target_info = link
            .read_register(Port::DebugPort, 0x4)?;

        let target_info = parse_target_id(target_info);
        println!("\nTarget Identification Register (TARGETID):");
        println!(
            "\tRevision = {}, Part Number = {}, Designer = {}",
            target_info.0, target_info.3, target_info.2
        );

        */


        // Note: Temporary read to ensure the DP information is read at
        //       least once before reading the ROM table 
        //       (necessary according to STM manual).
        //
        // TODO: Move to proper place somewhere in init code
        //

        let target_info = session.probe.read_register_dp(0x0)?;
        println!("DP info: {:#08x}", target_info);


        println!("\nAvailable Access Ports:");

        for port in 0..255 { 
            let access_port = GenericAP::new(port);
            if access_port_is_valid(&mut session.probe, access_port) {

                let idr = session.probe.read_register_ap(access_port, IDR::default())?;
                println!("{:#x?}", idr);

                if idr.CLASS == APClass::MEMAP {
                    let access_port: MemoryAP = access_port.into();

                    let base_register = session.probe.read_register_ap(access_port, BASE::default())?;

                    let mut baseaddr = if BaseaddrFormat::ADIv5 == base_register.Format {
                        let base2 = session.probe.read_register_ap(access_port, BASE2::default())?;
                        (u64::from(base2.BASEADDR) << 32)
                    } else { 0 };
                    baseaddr |= u64::from(base_register.BASEADDR << 12);
                    
                    let link_ref = &mut session.probe;

                    let component_table = CSComponent::try_parse(&link_ref.into(), baseaddr as u64);


                    component_table.iter().for_each(|entry| println!("{:#08x?}", entry));

                    // let mut reader = crate::memory::romtable::RomTableReader::new(&link_ref, baseaddr as u64);

                    // for e in reader.entries() {
                    //     if let Ok(e) = e {
                    //         println!("ROM Table Entry: Component @ 0x{:08x}", e.component_addr());
                    //     }
                    // }
                }
            }
        }

        Ok(())
    })
}
