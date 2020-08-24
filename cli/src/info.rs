use crate::{common::open_probe, SharedOptions};

use probe_rs::{
    architecture::arm::{
        ap::{valid_access_ports, APClass, BaseaddrFormat, MemoryAP, BASE, BASE2, IDR},
        m0::Demcr,
        memory::Component,
        ArmCommunicationInterface, ArmCommunicationInterfaceState,
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

    let mut state = ArmCommunicationInterfaceState::new();
    let mut interface = ArmCommunicationInterface::new(&mut probe, &mut state)?;

    if let Some(interface) = &mut interface {
        println!("\nAvailable Access Ports:");

        for access_port in valid_access_ports(interface) {
            let idr = interface.read_ap_register(access_port, IDR::default())?;
            println!("{:#x?}", idr);

            if idr.CLASS == APClass::MEMAP {
                let access_port: MemoryAP = access_port.into();

                let base_register = interface.read_ap_register(access_port, BASE::default())?;

                if !base_register.present {
                    // No debug entry present
                    println!("No debug entry present.");
                    continue;
                }

                let mut baseaddr = if BaseaddrFormat::ADIv5 == base_register.Format {
                    let base2 = interface.read_ap_register(access_port, BASE2::default())?;
                    u64::from(base2.BASEADDR) << 32
                } else {
                    0
                };
                baseaddr |= u64::from(base_register.BASEADDR << 12);

                let mut memory = interface.reborrow().memory_interface(access_port)?;

                // Enable
                // - Data Watchpoint and Trace (DWT)
                // - Instrumentation Trace Macrocell (ITM)
                // - Embedded Trace Macrocell (ETM)
                // - Trace Port Interface Unit (TPIU).
                let mut demcr = Demcr(memory.read_word_32(Demcr::ADDRESS)?);
                demcr.set_dwtena(true);
                memory.write_word_32(Demcr::ADDRESS, demcr.into())?;

                let component_table = Component::try_parse(&mut memory, baseaddr as u64);

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
        }
    } else {
        println!(
            "No DAP interface was found on the connected probe. Thus, ARM info cannot be printed."
        )
    }

    Ok(())
}
