use std::error::Error;
use std::fmt::Write;

use probe_rs::{
    architecture::{
        arm::{
            ap::{GenericAp, MemoryAp},
            armv6m::Demcr,
            component::Scs,
            dp::{DPIDR, TARGETID},
            memory::{Component, CoresightComponent, PeripheralType},
            sequences::DefaultArmSequence,
            ApAddress, ApInformation, ArmProbeInterface, DpAddress, MemoryApInformation, Register,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
    },
    MemoryMappedRegister, Probe, WireProtocol,
};

use anyhow::Result;
use probe_rs_cli_util::common_options::ProbeOptions;
use termtree::Tree;

pub(crate) fn show_info_of_device(common: &ProbeOptions) -> Result<()> {
    let mut probe = common.attach_probe()?;

    let protocols = if let Some(protocol) = common.protocol {
        vec![protocol]
    } else {
        vec![WireProtocol::Jtag, WireProtocol::Swd]
    };

    for protocol in protocols {
        println!("Probing target via {}", protocol);
        let (new_probe, result) = try_show_info(probe, protocol, common.connect_under_reset);

        probe = new_probe;

        probe.detach()?;

        if let Err(e) = result {
            println!(
                "Error identifying target using protocol {}: {}",
                protocol, e
            );
        }
    }

    Ok(())
}

fn try_show_info(
    mut probe: Probe,
    protocol: WireProtocol,
    connect_under_reset: bool,
) -> (Probe, Result<()>) {
    if let Err(e) = probe.select_protocol(protocol) {
        return (probe, Err(e.into()));
    }

    let attach_result = if connect_under_reset {
        probe.attach_to_unspecified_under_reset()
    } else {
        probe.attach_to_unspecified()
    };

    if let Err(e) = attach_result {
        return (probe, Err(e.into()));
    }

    let mut probe = probe;

    if probe.has_arm_interface() {
        match probe.try_into_arm_interface() {
            Ok(interface) => {
                match interface.initialize(DefaultArmSequence::create()) {
                    Ok(mut interface) => {
                        if let Err(e) = show_arm_info(&mut *interface) {
                            // Log error?
                            println!("Error showing ARM chip information: {}", e);
                        }

                        probe = interface.close();
                    }
                    Err((interface, e)) => {
                        println!("Error showing ARM chip information: {}", e);

                        probe = interface.close();
                    }
                }
            }
            Err((interface_probe, _e)) => {
                probe = interface_probe;
            }
        }
    } else {
        println!(
            "No DAP interface was found on the connected probe. Thus, ARM info cannot be printed."
        );
    }

    if protocol == WireProtocol::Jtag {
        if probe.has_riscv_interface() {
            match probe.try_into_riscv_interface() {
                Ok(mut interface) => {
                    if let Err(e) = show_riscv_info(&mut interface) {
                        log::warn!("Error showing RISCV chip information: {}", e);
                    }

                    probe = interface.close();
                }
                Err((interface_probe, e)) => {
                    let mut source = Some(&e as &dyn Error);

                    while let Some(parent) = source {
                        log::error!("Error: {}", parent);
                        source = parent.source();
                    }

                    probe = interface_probe;
                }
            }
        } else {
            println!(
            "Unable to debug RISC-V targets using the current probe. RISC-V specific information cannot be printed."
        );
        }
    } else {
        tracing::info!("Debugging RISCV-Targets over SWD is not supported.");
    }

    (probe, Ok(()))
}

fn show_arm_info(interface: &mut dyn ArmProbeInterface) -> Result<()> {
    let dp_info = interface.read_raw_dp_register(DpAddress::Default, DPIDR::ADDRESS)?;
    let dp_info = DPIDR(dp_info);

    let mut dp_node = String::new();

    write!(dp_node, "Debug Port: Version {}", dp_info.version())?;

    if dp_info.min() {
        write!(dp_node, ", MINDP")?;
    }

    let jep_code = jep106::JEP106Code::new(dp_info.jep_cc(), dp_info.jep_id());

    if dp_info.version() == 2 {
        let target_id = interface.read_raw_dp_register(DpAddress::Default, TARGETID::ADDRESS)?;

        let target_id = TARGETID(target_id);

        let part_no = target_id.tpartno();
        let revision = target_id.trevision();

        let designer_id = target_id.tdesigner();

        let cc = (designer_id >> 7) as u8;
        let id = (designer_id & 0x7f) as u8;

        let designer = jep106::JEP106Code::new(cc, id);

        write!(
            dp_node,
            ", Designer: {}",
            designer.get().unwrap_or("<unknown>")
        )?;
        write!(dp_node, ", Part: {:#x}", part_no)?;
        write!(dp_node, ", Revision: {:#x}", revision)?;
    } else {
        write!(
            dp_node,
            ", DP Designer: {}",
            jep_code.get().unwrap_or("<unknown>")
        )?;
    }

    let mut tree = Tree::new(dp_node);

    let dp = DpAddress::Default;
    let num_access_ports = interface.num_access_ports(dp).unwrap();

    for ap_index in 0..num_access_ports {
        let ap = ApAddress {
            ap: ap_index as u8,
            dp,
        };
        let access_port = GenericAp::new(ap);

        let ap_information = interface.ap_information(access_port).unwrap();

        match ap_information {
            ApInformation::MemoryAp(MemoryApInformation {
                debug_base_address,
                address,
                device_enabled,
                ..
            }) => {
                let mut ap_nodes = Tree::new(format!("{} MemoryAP", address.ap));

                if *device_enabled {
                    match handle_memory_ap(access_port.into(), *debug_base_address, interface) {
                        Ok(component_tree) => ap_nodes.push(component_tree),
                        Err(e) => ap_nodes.push(format!("Error during access: {}", e)),
                    };
                } else {
                    ap_nodes.push("Access disabled".to_string());
                }

                tree.push(ap_nodes);
            }

            ApInformation::Other { address, idr } => {
                let designer = idr.DESIGNER;

                let cc = (designer >> 7) as u8;
                let id = (designer & 0x7f) as u8;

                let jep = jep106::JEP106Code::new(cc, id);

                let ap_type = if designer == 0x43b {
                    format!("{:?}", idr.TYPE)
                } else {
                    format!("{:#x}", idr.TYPE as u8)
                };

                tree.push(format!(
                    "{} Unknown AP (Designer: {}, Class: {:?}, Type: {}, Variant: {:#x}, Revision: {:#x})",
                    address.ap,
                    jep.get().unwrap_or("<unknown>"),
                    idr.CLASS,
                    ap_type,
                    idr.VARIANT,
                    idr.REVISION
                ));
            }
        }
    }

    println!("{}", tree);

    Ok(())
}

fn handle_memory_ap(
    access_port: MemoryAp,
    base_address: u64,
    interface: &mut dyn ArmProbeInterface,
) -> Result<Tree<String>, anyhow::Error> {
    let component = {
        let mut memory = interface.memory_interface(access_port)?;
        let mut demcr = Demcr(memory.read_word_32(Demcr::ADDRESS)?);
        demcr.set_dwtena(true);
        memory.write_word_32(Demcr::ADDRESS, demcr.into())?;
        Component::try_parse(&mut *memory, base_address)?
    };
    let component_tree = coresight_component_tree(interface, component, access_port)?;

    Ok(component_tree)
}

fn coresight_component_tree(
    interface: &mut dyn ArmProbeInterface,
    component: Component,
    access_port: MemoryAp,
) -> Result<Tree<String>> {
    let tree = match &component {
        Component::GenericVerificationComponent(_) => Tree::new("Generic".to_string()),
        Component::Class1RomTable(_, table) => {
            let mut rom_table = Tree::new("ROM Table (Class 1)".to_string());

            for entry in table.entries() {
                let component = entry.component().clone();

                rom_table.push(coresight_component_tree(interface, component, access_port)?);
            }

            rom_table
        }
        Component::CoresightComponent(id) => {
            let peripheral_id = id.peripheral_id();

            let component_description = if let Some(part_info) = peripheral_id.determine_part() {
                format!("{: <15} (Coresight Component)", part_info.name())
            } else {
                format!(
                    "Coresight Component, Part: {:#06x}, Devtype: {:#04x}, Archid: {:#06x}, Designer: {}",
                    peripheral_id.part(),
                    peripheral_id.dev_type(),
                    peripheral_id.arch_id(),
                    peripheral_id
                        .jep106()
                        .and_then(|j| j.get())
                        .unwrap_or("<unknown>"),
                )
            };

            Tree::new(component_description)
        }

        Component::PeripheralTestBlock(_) => Tree::new("Peripheral test block".to_string()),
        Component::GenericIPComponent(id) => {
            let peripheral_id = id.peripheral_id();

            let desc = if let Some(part_desc) = peripheral_id.determine_part() {
                format!("{: <15} (Generic IP component)", part_desc.name())
            } else {
                "Generic IP component".to_string()
            };

            let mut tree = Tree::new(desc);

            if peripheral_id.is_of_type(PeripheralType::Scs) {
                let cc = &CoresightComponent::new(component, access_port);
                let scs = &mut Scs::new(interface, cc);
                let cpu_tree = cpu_info_tree(scs)?;

                tree.push(cpu_tree);
            }

            tree
        }

        Component::CoreLinkOrPrimeCellOrSystemComponent(_) => {
            Tree::new("Core Link / Prime Cell / System component".to_string())
        }
    };

    Ok(tree)
}

fn cpu_info_tree(scs: &mut Scs) -> Result<Tree<String>> {
    let mut tree = Tree::new("CPUID".into());

    let cpuid = scs.cpuid()?;

    let implementer = cpuid.implementer();
    let implementer = if implementer == 0x41 {
        "ARM Ltd".into()
    } else {
        implementer.to_string()
    };

    tree.push(format!("IMPLEMENTER: {}", implementer));
    tree.push(format!("VARIANT: {}", cpuid.variant()));
    tree.push(format!("PARTNO: {}", cpuid.partno()));
    tree.push(format!("REVISION: {}", cpuid.revision()));

    Ok(tree)
}

fn show_riscv_info(interface: &mut RiscvCommunicationInterface) -> Result<()> {
    let idcode = interface.read_idcode()?;

    let version = (idcode >> 28) & 0xf;
    let part_number = (idcode >> 12) & 0xffff;
    let manufacturer_id = (idcode >> 1) & 0x7ff;

    let jep_cc = (manufacturer_id >> 7) & 0xf;
    let jep_id = manufacturer_id & 0x3f;

    let jep_id = jep106::JEP106Code::new(jep_cc as u8, jep_id as u8);

    println!("RISCV Chip:");
    println!("\tIDCODE: {:010x}", idcode);
    println!("\t Version:      {}", version);
    println!("\t Part:         {}", part_number);
    println!("\t Manufacturer: {} ({})", manufacturer_id, jep_id);

    Ok(())
}
