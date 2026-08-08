use std::{fmt::Write, num::ParseIntError};

use anyhow::Result;
use jep106::JEP106Code;
use probe_rs::{
    architecture::arm::{
        ap::IDR,
        dp::{DLPIDR, TARGETID},
    },
    probe::WireProtocol,
};
use termtree::Tree;

use crate::rpc::functions::chip::convert::from_wire_jep106_code;
use crate::rpc::functions::probe::convert::{to_wire_debug_probe_selector, to_wire_protocol};
use crate::util::{cli::select_probe, common_options::ProbeOptions};
use probe_rs_rpc::info::{
    ApInfo, ComponentTreeNode, DebugPortInfo, DebugPortInfoNode, DebugPortVersion, InfoEvent,
    MinDpSupport, TargetInfoRequest,
};
use probe_rs_rpc_client::RpcClient;

const JEP_ARM: JEP106Code = JEP106Code::new(4, 0x3b);

#[derive(clap::Parser)]
pub struct Cmd {
    #[clap(flatten)]
    common: ProbeOptions,

    #[arg(short, long)]
    /// Enumerate all debug ports and components on the target.
    ///
    /// By default, the `info` subcommand attempts to autodetect the target device from the
    /// registry of known chips. Use the `--verbose` flag to discover more information about the
    /// chip, or to print information about chips that cannot be auto-detected.
    verbose: bool,

    /// SWD Multidrop target selection value for --verbose mode
    ///
    /// If provided, this value is written into the debug port TARGETSEL register
    /// when connecting. This is required in --verbose mode for targets using SWD multidrop.
    #[arg(long, value_parser = parse_hex, requires = "verbose")]
    target_sel: Option<u32>,
    /// Override JTAG scan chain IR lengths for --verbose mode (bypasses auto-detection)
    ///
    /// Specify one or more IR lengths (in bits) for each TAP in the chain, in scan-chain order.
    /// For example, `--scan-chain 5` for a single-TAP chain with IR length 5.
    /// When set, the normal JTAG auto-detection DR/IR scan is skipped entirely.
    #[arg(
        long,
        value_delimiter = ',',
        value_name = "IR_LEN",
        requires = "verbose"
    )]
    scan_chain: Vec<u8>,
}

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(src)
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        if self.verbose {
            let protocols = if let Some(protocol) = self.common.protocol {
                vec![protocol]
            } else {
                vec![WireProtocol::Jtag, WireProtocol::Swd]
            };

            let probe = select_probe(
                &client,
                self.common.probe.map(to_wire_debug_probe_selector),
                self.common.non_interactive,
            )
            .await?;

            let mut any_success = false;

            for protocol in protocols {
                let msg = format!("Probing target via {protocol}");
                println!("{msg}");
                println!("{}", "-".repeat(msg.len()));
                println!();

                let mut successes = vec![];
                let mut errors = vec![];

                let req = TargetInfoRequest {
                    target_sel: self.target_sel,
                    protocol: to_wire_protocol(protocol),

                    probe: probe.clone(),
                    speed: self.common.speed,
                    connect_under_reset: self.common.connect_under_reset,
                    dry_run: self.common.dry_run,
                    scan_chain: self.scan_chain.clone(),
                };

                let result = client
                    .info(req, async |message| {
                        let is_success =
                            matches!(message, InfoEvent::Idcode { .. } | InfoEvent::ArmDp(_));

                        if matches!(message, InfoEvent::Message(_)) {
                            successes.push(message.clone());
                            errors.push(message.clone());
                        }

                        if is_success {
                            successes.push(message);
                        } else {
                            errors.push(message);
                        }
                    })
                    .await;

                if let Err(error) = result {
                    println!("Error while probing target: {error}");
                }

                if successes.is_empty() {
                    for message in errors {
                        println!("{}", format_info_event(&message));
                    }
                } else {
                    any_success = true;
                    for message in successes {
                        println!("{}", format_info_event(&message));
                    }
                }
            }

            if !any_success {
                println!();
                println!(
                    "Note: `info` only tries to identify the debug port and its components. \
                     A failed or incomplete result does not necessarily mean the chip or your \
                     wiring is broken - flashing and debugging may still work fine."
                );
            }
        } else {
            match crate::cmd::common::info::basic_info(&client, self.common).await {
                Ok(info) => {
                    println!("Detected chip: {}", info.chip);
                    println!(
                        "For more detailed information about the target, run with the --verbose flag."
                    );
                }
                Err(e) => {
                    eprintln!(
                        "Could not attach to target. Try running with --verbose for more information about the target."
                    );
                    return Err(e);
                }
            };
        }

        Ok(())
    }
}

fn format_info_event(event: &InfoEvent) -> String {
    let mut output = String::new();
    match event {
        InfoEvent::Message(message) => {
            writeln!(output, "{message}").unwrap();
        }
        InfoEvent::ProtocolNotSupportedByArch {
            architecture,
            protocol,
        } => {
            writeln!(
                output,
                "Debugging {architecture} targets over {protocol} is not supported. {architecture} specific information cannot be printed."
            )
            .unwrap();
        }
        InfoEvent::ProbeInterfaceMissing {
            interface,
            architecture,
        } => {
            writeln!(
                output,
                "No {interface} interface was found on the connected probe. {architecture} specific information cannot be printed."
            )
            .unwrap();
        }
        InfoEvent::Error {
            architecture,
            error,
        } => {
            writeln!(
                output,
                "Error showing {architecture} chip information: {error}"
            )
            .unwrap();
        }
        InfoEvent::ArmError { dp_addr, error } => {
            writeln!(
                output,
                "Error showing ARM chip information for Debug Port {dp_addr:?}: {error}",
            )
            .unwrap();
        }
        InfoEvent::Idcode {
            architecture,
            idcode: Some(idcode),
        } => {
            let version = (idcode >> 28) & 0xf;
            let part_number = (idcode >> 12) & 0xffff;
            let manufacturer_id = (idcode >> 1) & 0x7ff;

            let jep_cc = (manufacturer_id >> 7) & 0xf;
            let jep_id = manufacturer_id & 0x7f;

            let jep_id = jep106::JEP106Code::new(jep_cc as u8, jep_id as u8);

            writeln!(output, "{architecture} Chip:").unwrap();
            writeln!(output, "  IDCODE: {idcode:010x}").unwrap();
            writeln!(output, "    Version:      {version}").unwrap();
            writeln!(output, "    Part:         {part_number}").unwrap();
            writeln!(output, "    Manufacturer: {manufacturer_id} ({jep_id})").unwrap();
        }
        InfoEvent::Idcode {
            architecture,
            idcode: None,
        } => {
            writeln!(output, "No IDCODE info for this {architecture} chip.").unwrap();
        }
        InfoEvent::ArmDp(dp_info) => {
            writeln!(output, "{}", format_debug_port_info(dp_info)).unwrap();
        }
    }
    output
}

fn component_tree_to_termtree(node: &ComponentTreeNode) -> Tree<String> {
    let mut tree = Tree::new(node.node.clone());

    for child in node.children.iter() {
        tree.push(component_tree_to_termtree(child));
    }

    tree
}

fn format_debug_port_info_node(node: &DebugPortInfoNode) -> String {
    fn format_jep(jep: JEP106Code) -> String {
        format!("Designer: {}", jep.get().unwrap_or("<unknown>"))
    }

    let mut output = String::new();
    write!(
        output,
        "Debug Port: {}",
        match node.dp_info.version {
            DebugPortVersion::DPv0 => "DPv0".to_string(),
            DebugPortVersion::DPv1 => "DPv1".to_string(),
            DebugPortVersion::DPv2 => "DPv2".to_string(),
            DebugPortVersion::DPv3 => "DPv3".to_string(),
            DebugPortVersion::Unsupported(version) =>
                format!("<unsupported Debugport Version {version}>"),
        }
    )
    .unwrap();

    if node.dp_info.min_dp_support == MinDpSupport::Implemented {
        write!(output, ", MINDP").unwrap();
    }

    if node.dp_info.version == DebugPortVersion::DPv2 {
        let target_id = TARGETID(node.targetid);
        let dlpidr = DLPIDR(node.dlpidr);

        let part_no = target_id.tpartno();
        let revision = target_id.trevision();

        let designer_id = target_id.tdesigner();

        let cc = (designer_id >> 7) as u8;
        let id = (designer_id & 0x7f) as u8;

        let designer = jep106::JEP106Code::new(cc, id);

        write!(output, ", {}", format_jep(designer)).unwrap();
        write!(output, ", Part: {part_no:#x}").unwrap();
        write!(output, ", Revision: {revision:#x}").unwrap();

        let instance = dlpidr.tinstance();

        write!(output, ", Instance: {instance:#04x}").unwrap();
    } else {
        write!(
            output,
            ", {}",
            format_jep(from_wire_jep106_code(node.dp_info.designer))
        )
        .unwrap();
    }

    output
}

fn format_debug_port_info(info: &DebugPortInfo) -> String {
    let mut tree = Tree::new(format_debug_port_info_node(&info.dp_info));
    if info.aps.is_empty() {
        tree.push(Tree::new("No access ports found on this chip.".to_string()));
    } else {
        for ap in &info.aps {
            match ap {
                ApInfo::MemoryAp {
                    ap_addr,
                    component_tree,
                } => {
                    let mut ap_root = Tree::new(format!("{} MemoryAP", ap_addr.ap));

                    ap_root.push(component_tree_to_termtree(component_tree));

                    tree.push(ap_root);
                }
                ApInfo::ApV2Root { component_tree } => {
                    for child in component_tree.children.iter() {
                        tree.push(component_tree_to_termtree(child));
                    }
                }
                ApInfo::Unknown { ap_addr, idr } => {
                    let idr = IDR::from_raw(*idr);
                    let jep = idr.DESIGNER();

                    let ap_type = if jep == JEP_ARM {
                        format!("{:?}", idr.TYPE())
                    } else {
                        format!("{:#x}", u32::from(idr) & 0xF)
                    };

                    let ap_node = Tree::new(format!(
                        "{} Unknown AP (Designer: {}, Class: {:?}, Type: {}, Variant: {:#x}, Revision: {:#x})",
                        ap_addr.ap,
                        jep.get().unwrap_or("<unknown>"),
                        idr.CLASS(),
                        ap_type,
                        idr.VARIANT(),
                        idr.REVISION()
                    ));

                    tree.push(ap_node);
                }
            };
        }
    }

    format!("{tree}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn jep_arm_is_arm() {
        assert_eq!(super::JEP_ARM.get(), Some("ARM Ltd"))
    }
}
