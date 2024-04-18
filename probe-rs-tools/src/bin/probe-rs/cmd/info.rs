use std::fmt::Write;

use anyhow::{anyhow, Result};
use jep106::JEP106Code;
use probe_rs::{
    architecture::{
        arm::{
            ap::{GenericAp, MemoryAp},
            armv6m::Demcr,
            component::Scs,
            dp::{DebugPortId, DebugPortVersion, MinDpSupport, DLPIDR, DPIDR, TARGETID},
            memory::{Component, CoresightComponent, PeripheralType},
            sequences::DefaultArmSequence,
            ApAddress, ApInformation, ArmProbeInterface, DpAddress, MemoryApInformation, Register,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::{XtensaCommunicationInterface, XtensaSaveState},
    },
    probe::{list::Lister, Probe, WireProtocol},
    MemoryMappedRegister,
};
use termtree::Tree;

use crate::util::common_options::ProbeOptions;

const JEP_ARM: JEP106Code = JEP106Code::new(4, 0x3b);

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,
    /// SWD Multidrop target selection value
    ///
    /// If provided, this value is written into the debug port TARGETSEL register
    /// when connecting. This is required for targets using SWD multidrop
    #[arg(long, value_parser = parse_hex)]
    target_sel: Option<u32>,
}

// Clippy doesn't like `from_str_radix` with radix 10, but I prefer the symmetry`
// with the hex case.
#[allow(clippy::from_str_radix_10)]
fn parse_hex(src: &str) -> Result<u32, std::num::ParseIntError> {
    if src.starts_with("0x") {
        u32::from_str_radix(src.trim_start_matches("0x"), 16)
    } else {
        u32::from_str_radix(src, 10)
    }
}

impl Cmd {
    pub fn run(self, lister: &Lister) -> anyhow::Result<()> {
        let probe_options = self.common.load()?;
        let mut probe = probe_options.attach_probe(lister)?;

        let protocols = if let Some(protocol) = probe_options.protocol() {
            vec![protocol]
        } else {
            vec![WireProtocol::Jtag, WireProtocol::Swd]
        };

        for protocol in protocols {
            println!("Probing target via {protocol}");
            println!();

            let (new_probe, result) = try_show_info(
                probe,
                protocol,
                probe_options.connect_under_reset(),
                self.target_sel,
            );

            probe = new_probe;

            probe.detach()?;

            if let Err(e) = result {
                println!("Error identifying target using protocol {protocol}: {e}");
            }

            println!();
        }

        Ok(())
    }
}

const ALTERNATE_DP_ADRESSES: [DpAddress; 2] = [
    DpAddress::Multidrop(0x01002927),
    DpAddress::Multidrop(0x11002927),
];

fn try_show_info(
    mut probe: Probe,
    protocol: WireProtocol,
    connect_under_reset: bool,
    target_sel: Option<u32>,
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

    if probe.has_arm_interface() {
        let dp_addr = if let Some(target_sel) = target_sel {
            DpAddress::Multidrop(target_sel)
        } else {
            DpAddress::Default
        };

        let print_err = |dp_addr, e| {
            println!(
                "Error showing ARM chip information for Debug Port {:?}: {:?}",
                dp_addr, e
            );
            println!();
        };
        match try_show_arm_dp_info(probe, dp_addr) {
            (probe_moved, Ok(_)) => probe = probe_moved,
            (probe_moved, Err(e)) => {
                probe = probe_moved;
                print_err(dp_addr, e);

                if dp_addr == DpAddress::Default {
                    println!("Trying alternate multi-drop debug ports");

                    for address in ALTERNATE_DP_ADRESSES {
                        match try_show_arm_dp_info(probe, address) {
                            (probe_moved, Ok(dp_version)) => {
                                probe = probe_moved;
                                if dp_version < DebugPortVersion::DPv2 {
                                    println!("Debug port version {} does not support SWD multidrop. Stopping here.", dp_version);
                                    break;
                                }
                            }
                            (probe_moved, Err(e)) => {
                                probe = probe_moved;
                                print_err(address, e);
                            }
                        }
                    }
                }
            }
        }
    } else {
        println!("No DAP interface was found on the connected probe. ARM-specific information cannot be printed.");
    }

    // This check is a bit weird, but `try_into_riscv_interface` will try to switch the protocol to JTAG.
    // If the current protocol we want to use is SWD, we have avoid this.
    if probe.has_riscv_interface() && protocol == WireProtocol::Jtag {
        tracing::debug!("Trying to show RISC-V chip information");
        match probe.try_get_riscv_interface_factory() {
            Ok(factory) => {
                let mut state = factory.create_state();
                match factory.attach(&mut state) {
                    Ok(mut interface) => {
                        if let Err(e) = show_riscv_info(&mut interface) {
                            println!("Error showing RISC-V chip information: {:?}", anyhow!(e));
                        }
                    }
                    Err(e) => println!(
                        "Error while attaching to RISC-V interface: {:?}",
                        anyhow!(e)
                    ),
                };
            }
            Err(e) => println!("Error while reading RISC-V info: {:?}", anyhow!(e)),
        }
    } else if protocol == WireProtocol::Swd {
        println!(
            "Debugging RISC-V targets over SWD is not supported. For these targets, JTAG is the only supported protocol. RISC-V specific information cannot be printed."
        );
    } else {
        println!(
            "Unable to debug RISC-V targets using the current probe. RISC-V specific information cannot be printed."
        );
    }

    // This check is a bit weird, but `try_into_xtensa_interface` will try to switch the protocol to JTAG.
    // If the current protocol we want to use is SWD, we have avoid this.
    if probe.has_xtensa_interface() && protocol == WireProtocol::Jtag {
        tracing::debug!("Trying to show Xtensa chip information");
        let mut state = XtensaSaveState::default();
        match probe.try_get_xtensa_interface(&mut state) {
            Ok(mut interface) => {
                if let Err(e) = show_xtensa_info(&mut interface) {
                    println!("Error showing Xtensa chip information: {:?}", anyhow!(e));
                }
            }
            Err(e) => {
                println!("Error showing Xtensa chip information: {:?}", anyhow!(e));
            }
        }
    } else if protocol == WireProtocol::Swd {
        println!(
            "Debugging Xtensa targets over SWD is not supported. For these targets, JTAG is the only supported protocol. Xtensa specific information cannot be printed."
        );
    } else {
        println!(
            "Unable to debug Xtensa targets using the current probe. Xtensa specific information cannot be printed."
        );
    }

    (probe, Ok(()))
}

fn try_show_arm_dp_info(probe: Probe, dp_address: DpAddress) -> (Probe, Result<DebugPortVersion>) {
    tracing::debug!("Trying to show ARM chip information");
    match probe
        .try_into_arm_interface()
        .map_err(|(iface, e)| (iface, anyhow!(e)))
        .and_then(|interface| {
            interface
                .initialize(DefaultArmSequence::create(), dp_address)
                .map_err(|(interface, e)| (interface.close(), anyhow!(e)))
        }) {
        Ok(mut interface) => {
            let res = show_arm_info(&mut *interface, dp_address);
            (interface.close(), res)
        }
        Err((probe, e)) => (probe, Err(e)),
    }
}

/// Try to show information about the ARM chip, connected to a DP at the given address.
///
/// Returns the version of the DP.
fn show_arm_info(interface: &mut dyn ArmProbeInterface, dp: DpAddress) -> Result<DebugPortVersion> {
    let dp_info = interface.read_raw_dp_register(dp, DPIDR::ADDRESS)?;
    let dp_info = DebugPortId::from(DPIDR(dp_info));

    let mut dp_node = String::new();

    write!(dp_node, "Debug Port: {}", dp_info.version)?;

    if dp_info.min_dp_support == MinDpSupport::Implemented {
        write!(dp_node, ", MINDP")?;
    }

    if dp_info.version == DebugPortVersion::DPv2 {
        let target_id = interface.read_raw_dp_register(dp, TARGETID::ADDRESS)?;

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
        write!(dp_node, ", Part: {part_no:#x}")?;
        write!(dp_node, ", Revision: {revision:#x}")?;

        // Read Instance ID
        let dlpidr = DLPIDR(interface.read_raw_dp_register(dp, DLPIDR::ADDRESS)?);

        let instance = dlpidr.tinstance();

        write!(dp_node, ", Instance: {:#04x}", instance)?;
    } else {
        write!(
            dp_node,
            ", DP Designer: {}",
            dp_info.designer.get().unwrap_or("<unknown>")
        )?;
    }

    let mut tree = Tree::new(dp_node);

    let num_access_ports = interface.num_access_ports(dp)?;

    for ap_index in 0..num_access_ports {
        let ap = ApAddress {
            ap: ap_index as u8,
            dp,
        };
        let access_port = GenericAp::new(ap);

        let ap_information = interface.ap_information(access_port)?;

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
                        Err(e) => ap_nodes.push(format!("Error during access: {e}")),
                    };
                } else {
                    ap_nodes.push("Access disabled".to_string());
                }

                tree.push(ap_nodes);
            }

            ApInformation::Other { address, idr } => {
                let jep = idr.DESIGNER;

                let ap_type = if idr.DESIGNER == JEP_ARM {
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

    println!("ARM Chip with debug port {:x?}:", dp);
    println!("{tree}");

    if num_access_ports == 0 {
        println!("No access ports found on this chip.");
    }
    println!();

    Ok(dp_info.version)
}

fn handle_memory_ap(
    access_port: MemoryAp,
    base_address: u64,
    interface: &mut dyn ArmProbeInterface,
) -> Result<Tree<String>, anyhow::Error> {
    let component = {
        let mut memory = interface.memory_interface(access_port)?;
        let mut demcr = Demcr(memory.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_dwtena(true);
        memory.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
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

    tree.push(format!("IMPLEMENTER: {implementer}"));
    tree.push(format!("VARIANT: {}", cpuid.variant()));
    tree.push(format!("PARTNO: {}", cpuid.partno()));
    tree.push(format!("REVISION: {}", cpuid.revision()));

    Ok(tree)
}

fn show_riscv_info(interface: &mut RiscvCommunicationInterface) -> Result<()> {
    if let Some(idcode) = interface.read_idcode()? {
        print_idcode_info("RISC-V", idcode);
    } else {
        println!("No IDCODE info for this RISC-V chip.")
    }

    Ok(())
}

fn show_xtensa_info(interface: &mut XtensaCommunicationInterface) -> Result<()> {
    let idcode = interface.read_idcode()?;

    print_idcode_info("Xtensa", idcode);

    Ok(())
}

fn print_idcode_info(architecture: &str, idcode: u32) {
    let version = (idcode >> 28) & 0xf;
    let part_number = (idcode >> 12) & 0xffff;
    let manufacturer_id = (idcode >> 1) & 0x7ff;

    let jep_cc = (manufacturer_id >> 7) & 0xf;
    let jep_id = manufacturer_id & 0x7f;

    let jep_id = jep106::JEP106Code::new(jep_cc as u8, jep_id as u8);

    println!("{architecture} Chip:");
    println!("  IDCODE: {idcode:010x}");
    println!("    Version:      {version}");
    println!("    Part:         {part_number}");
    println!("    Manufacturer: {manufacturer_id} ({jep_id})");
}

#[cfg(test)]
mod tests {
    #[test]
    fn jep_arm_is_arm() {
        assert_eq!(super::JEP_ARM.get(), Some("ARM Ltd"))
    }
}
