use crate::{common::open_probe, SharedOptions};

use probe_rs::{
    architecture::arm::{
        ap::{GenericAP, MemoryAP},
        m0::Demcr,
        memory::Component,
        ApInformation, MemoryApInformation,
    },
    CoreRegister,
};

use anyhow::Result;

pub(crate) fn show_info_of_device(shared_options: &SharedOptions) -> Result<()> {
    let mut probe = open_probe(shared_options.n)?;
    probe.attach_to_unspecified()?;

    /*
        The following code only works with debug port v2,
        which might not necessarily be present.

        Once the typed interface for the debug port is done, it
        can be enabled again.

    println!("Device information:");


    let target_info = link
        .read_register(PortType::DebugPort, 0x4)?;

    let target_info = parse_target_id(target_info);
    println!("\nTarget Identification Register (TARGETID):");
    println!(
        "\tRevision = {}, Part Number = {}, Designer = {}",
        target_info.0, target_info.3, target_info.2
    );

    */

    let mut interface = probe.into_arm_interface()?;

    if let Some(interface) = &mut interface {
        println!("\nAvailable Access Ports:");

        let num_access_ports = interface.num_access_ports();

        for ap_index in 0..num_access_ports {
            let access_port = GenericAP::from(ap_index as u8);

            let ap_information = interface.ap_information(access_port).unwrap();

            //let idr = interface.read_ap_register(access_port, IDR::default())?;
            //println!("{:#x?}", idr);

            match ap_information {
                ApInformation::MemoryAp(MemoryApInformation {
                    debug_base_address, ..
                }) => {
                    let access_port: MemoryAP = access_port.into();

                    let base_address = *debug_base_address;

                    let mut memory = interface.memory_interface(access_port)?;

                    // Enable
                    // - Data Watchpoint and Trace (DWT)
                    // - Instrumentation Trace Macrocell (ITM)
                    // - Embedded Trace Macrocell (ETM)
                    // - Trace Port Interface Unit (TPIU).
                    let mut demcr = Demcr(memory.read_word_32(Demcr::ADDRESS)?);
                    demcr.set_dwtena(true);
                    memory.write_word_32(Demcr::ADDRESS, demcr.into())?;

                    let component_table = Component::try_parse(&mut memory, base_address);

                    component_table
                        .iter()
                        .for_each(|entry| println!("{:#08x?}", entry));

                    // let mut reader = crate::memory::romtable::RomTableReader::new(&link_ref, baseaddr as u64);

                    // for e in reader.entries() {
                    //     if let Ok(e) = e {
                    //         println!("ROM Table Entry: Component @ 0x{:08x}", e.component_addr());
                    //     }
                    // }
                }
                ApInformation::Other { .. } => println!("Unknown Type of access port"),
            }
        }
    } else {
        println!(
            "No DAP interface was found on the connected probe. Thus, ARM info cannot be printed."
        )
    }

    Ok(())
}
