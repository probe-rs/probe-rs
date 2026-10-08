use std::{fmt::Write, num::ParseIntError};

use anyhow::Result;
use jep106::JEP106Code;
use probe_rs::{
    architecture::{
        arm::{
            ap::IDR,
            dp::{DLPIDR, TARGETID},
        },
        riscv::communication_interface::HartIsa,
    },
    probe::WireProtocol,
};
use termtree::Tree;

use crate::rpc::functions::chip::convert::from_wire_jep106_code;
use crate::rpc::functions::probe::convert::{to_wire_debug_probe_selector, to_wire_protocol};
use crate::util::{cli::select_probe, common_options::ProbeOptions};
use probe_rs_rpc::info::{
    ApInfo, ComponentTreeNode, DebugPortInfo, DebugPortInfoNode, DebugPortVersion, InfoEvent,
    JtagTapInfo, MinDpSupport, RiscvDebugModuleInfo, RiscvDebugModuleVersion, RiscvHartIsa,
    TargetInfoRequest,
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

                let mut events = vec![];
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
                        events.push(message.clone());

                        let is_success = is_success(&message);

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

                // The TAPs of a scanned chain are probed only for the architecture that their IR
                // length and IDCODE tell, so most errors there are not from a wrong guess.
                let scanned_chain = events
                    .iter()
                    .any(|event| matches!(event, InfoEvent::JtagScanChain(_)));

                if scanned_chain {
                    any_success |= events.iter().any(is_success);
                    println!("{}", format_jtag_scan_chain(&events));
                } else if successes.is_empty() {
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

fn is_success(event: &InfoEvent) -> bool {
    matches!(
        event,
        InfoEvent::Idcode { .. } | InfoEvent::ArmDp(_) | InfoEvent::RiscvDebugModule(_)
    )
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
            writeln!(output, "The chip is presumably not {architecture}.").unwrap();
        }
        InfoEvent::ArmDp(dp_info) => {
            writeln!(output, "{}", debug_port_info_tree(dp_info)).unwrap();
        }
        InfoEvent::JtagScanChain(taps) => {
            writeln!(output, "{}", jtag_scan_chain_tree(taps)).unwrap();
        }
        InfoEvent::JtagTap { index } => {
            writeln!(output, "TAP {index}:").unwrap();
        }
        InfoEvent::RiscvDebugModule(info) => {
            writeln!(output, "{}", riscv_debug_module_tree(info)).unwrap();
        }
    }
    output
}

fn jtag_scan_chain_tree(taps: &[JtagTapInfo]) -> Tree<String> {
    let mut tree = Tree::new(format!("JTAG scan chain with {} TAPs", taps.len()));
    for (index, tap) in taps.iter().enumerate() {
        tree.push(Tree::new(format!(
            "TAP {index}: {}, IR length: {}",
            format_idcode(tap.idcode),
            tap.ir_len
        )));
    }
    tree
}

/// Shows the events of a scanned JTAG chain as one tree, with the result of each TAP under it.
fn format_jtag_scan_chain(events: &[InfoEvent]) -> String {
    let mut output = String::new();
    let mut tree: Option<Tree<String>> = None;
    let mut current_tap = None;

    for event in events {
        let tap = current_tap.and_then(|index| tree.as_mut()?.leaves.get_mut(index));
        match (event, tap) {
            (InfoEvent::JtagScanChain(taps), _) => tree = Some(jtag_scan_chain_tree(taps)),
            (InfoEvent::JtagTap { index }, _) => current_tap = Some(*index as usize),
            (InfoEvent::ArmDp(info), Some(tap)) => {
                tap.push(debug_port_info_tree(info));
            }
            (InfoEvent::RiscvDebugModule(info), Some(tap)) => {
                tap.push(riscv_debug_module_tree(info));
            }
            // The IDCODE is already on the TAP.
            (
                InfoEvent::Idcode {
                    architecture,
                    idcode: Some(_),
                },
                Some(tap),
            ) => {
                tap.push(Tree::new(format!("{architecture} Chip")));
            }
            (event, Some(tap)) => {
                tap.push(Tree::new(format_info_event(event).trim_end().to_string()));
            }
            (event, None) => output.push_str(&format_info_event(event)),
        }
    }

    if let Some(tree) = tree {
        write!(output, "{tree}").unwrap();
    }
    output
}

fn format_idcode(idcode: Option<u32>) -> String {
    let Some(idcode) = idcode else {
        return "No IDCODE".to_string();
    };

    let version = (idcode >> 28) & 0xf;
    let part_number = (idcode >> 12) & 0xffff;
    let manufacturer_id = (idcode >> 1) & 0x7ff;
    let designer = JEP106Code::new((manufacturer_id >> 7) as u8, (manufacturer_id & 0x7f) as u8);

    format!(
        "IDCODE {idcode:#010x} (Designer: {}, Part: {part_number:#06x}, Version: {version})",
        designer.get().unwrap_or("<unknown>")
    )
}

fn riscv_debug_module_tree(info: &RiscvDebugModuleInfo) -> Tree<String> {
    let version = match info.version {
        RiscvDebugModuleVersion::NoModule => "none".to_string(),
        RiscvDebugModuleVersion::Version { major, minor } => format!("{major}.{minor}"),
        RiscvDebugModuleVersion::NonConforming => "non-conforming".to_string(),
        RiscvDebugModuleVersion::Unknown(version) => format!("<unknown version {version}>"),
    };

    let mut tree = Tree::new(format!(
        "RISC-V Debug Module (Version: {version}, Harts: {})",
        info.harts.len()
    ));
    for hart in &info.harts {
        let isa = match &hart.isa {
            RiscvHartIsa::Unavailable => "unavailable".to_string(),
            RiscvHartIsa::NotImplemented => "misa is not implemented".to_string(),
            RiscvHartIsa::Isa { xlen, extensions } => format_riscv_isa(&HartIsa {
                xlen: *xlen,
                extensions: *extensions,
            }),
            RiscvHartIsa::Error(error) => format!("Error reading misa: {error}"),
        };
        tree.push(Tree::new(format!("Hart {}: {isa}", hart.index)));
    }

    tree
}

fn format_riscv_isa(isa: &HartIsa) -> String {
    // The order of the single-letter extensions in an ISA string. `misa` uses S and U for the
    // privilege modes, and X for the presence of non-standard extensions.
    const ISA_STRING_ORDER: &str = "IEMAFDQLCBKJTPVNH";
    const NOT_IN_ISA_STRING: &str = "SUX";

    let mut output = match isa.xlen {
        Some(xlen) => format!("RV{xlen}"),
        None => "RV".to_string(),
    };
    output.extend(ISA_STRING_ORDER.chars().filter(|&e| isa.has_extension(e)));
    output.extend(('A'..='Z').filter(|&e| {
        isa.has_extension(e) && !ISA_STRING_ORDER.contains(e) && !NOT_IN_ISA_STRING.contains(e)
    }));

    if isa.xlen.is_none() {
        output.push_str(" (XLEN unknown)");
    }

    let mut modes = vec!["M"];
    if isa.has_extension('S') {
        modes.push("S");
    }
    if isa.has_extension('U') {
        modes.push("U");
    }
    write!(output, ", Privilege modes: {}", modes.join(", ")).unwrap();

    if isa.has_extension('X') {
        output.push_str(", Non-standard extensions");
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

fn debug_port_info_tree(info: &DebugPortInfo) -> Tree<String> {
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

    tree
}

#[cfg(test)]
mod tests {
    use probe_rs::architecture::riscv::communication_interface::HartIsa;

    #[test]
    fn jep_arm_is_arm() {
        assert_eq!(super::JEP_ARM.get(), Some("ARM Ltd"))
    }

    #[test]
    fn rv32imac_isa_string() {
        let isa = HartIsa::from_misa(0x4010_1105).unwrap();
        assert_eq!(
            super::format_riscv_isa(&isa),
            "RV32IMAC, Privilege modes: M, U"
        );
    }

    #[test]
    fn rv64gc_isa_string() {
        let isa = HartIsa::from_misa(0x8000_0000_0094_112d).unwrap();
        assert_eq!(
            super::format_riscv_isa(&isa),
            "RV64IMAFDC, Privilege modes: M, S, U, Non-standard extensions"
        );
    }

    #[test]
    fn isa_string_without_xlen() {
        let isa = HartIsa::from_misa(0x0000_0020_0000_1100).unwrap();
        assert_eq!(
            super::format_riscv_isa(&isa),
            "RVIM (XLEN unknown), Privilege modes: M"
        );
    }

    #[test]
    fn idcode_format() {
        assert_eq!(
            super::format_idcode(Some(0x4ba0_0477)),
            "IDCODE 0x4ba00477 (Designer: ARM Ltd, Part: 0xba00, Version: 4)"
        );
        assert_eq!(super::format_idcode(None), "No IDCODE");
    }
}
