use std::{fmt::Display, num::ParseIntError};

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

use crate::{
    rpc::{
        client::RpcClient,
        functions::info::{
            ApInfo, ComponentTreeNode, DebugPortInfo, DebugPortInfoNode, DebugPortVersion,
            InfoEvent, MinDpSupport, TargetInfoRequest,
        },
    },
    util::{cli::select_probe, common_options::ProbeOptions},
};

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

fn parse_hex(src: &str) -> Result<u32, ParseIntError> {
    parse_int::parse(src)
}

impl Cmd {
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let protocols = if let Some(protocol) = self.common.protocol {
            vec![protocol]
        } else {
            vec![WireProtocol::Jtag, WireProtocol::Swd]
        };

        let probe = select_probe(&client, self.common.probe.map(Into::into)).await?;

        for protocol in protocols {
            let msg = format!("Probing target via {protocol}");
            println!("{msg}");
            println!("{}", "-".repeat(msg.len()));
            println!();

            let mut successes = vec![];
            let mut errors = vec![];

            let req = TargetInfoRequest {
                target_sel: self.target_sel,
                protocol: protocol.into(),

                probe: probe.clone(),
                speed: self.common.speed,
                connect_under_reset: self.common.connect_under_reset,
                dry_run: self.common.dry_run,
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
                    println!("{message}");
                }
            } else {
                for message in successes {
                    println!("{message}");
                }
            }
        }

        Ok(())
    }
}

impl std::fmt::Display for InfoEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InfoEvent::Message(message) => writeln!(f, "{message}"),
            InfoEvent::ProtocolNotSupportedByArch {
                architecture,
                protocol,
            } => {
                writeln!(
                    f,
                    "Debugging {architecture} targets over {protocol} is not supported. {architecture} specific information cannot be printed."
                )
            }
            InfoEvent::ProbeInterfaceMissing {
                interface,
                architecture,
            } => {
                writeln!(
                    f,
                    "No {interface} interface was found on the connected probe. {architecture} specific information cannot be printed."
                )
            }
            InfoEvent::Error {
                architecture,
                error,
            } => {
                writeln!(f, "Error showing {architecture} chip information: {error}")
            }
            InfoEvent::ArmError { dp_addr, error } => {
                writeln!(
                    f,
                    "Error showing ARM chip information for Debug Port {dp_addr:?}: {error}",
                )
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

                writeln!(f, "{architecture} Chip:")?;
                writeln!(f, "  IDCODE: {idcode:010x}")?;
                writeln!(f, "    Version:      {version}")?;
                writeln!(f, "    Part:         {part_number}")?;
                writeln!(f, "    Manufacturer: {manufacturer_id} ({jep_id})")
            }
            InfoEvent::Idcode {
                architecture,
                idcode: None,
            } => {
                writeln!(f, "No IDCODE info for this {architecture} chip.")
            }
            InfoEvent::ArmDp(dp_info) => {
                writeln!(f, "{dp_info}")
            }
        }
    }
}

impl From<&ComponentTreeNode> for Tree<String> {
    fn from(node: &ComponentTreeNode) -> Self {
        let mut tree = Tree::new(node.node.clone());

        for child in node.children.iter() {
            tree.push(child);
        }

        tree
    }
}

impl Display for DebugPortInfoNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn format_jep(f: &mut std::fmt::Formatter<'_>, jep: JEP106Code) -> std::fmt::Result {
            write!(f, ", Designer: {}", jep.get().unwrap_or("<unknown>"))
        }

        write!(
            f,
            "Debug Port: {}",
            match self.dp_info.version {
                DebugPortVersion::DPv0 => "DPv0".to_string(),
                DebugPortVersion::DPv1 => "DPv1".to_string(),
                DebugPortVersion::DPv2 => "DPv2".to_string(),
                DebugPortVersion::DPv3 => "DPv3".to_string(),
                DebugPortVersion::Unsupported(version) =>
                    format!("<unsupported Debugport Version {version}>"),
            }
        )?;

        if self.dp_info.min_dp_support == MinDpSupport::Implemented {
            write!(f, ", MINDP")?;
        }

        if self.dp_info.version == DebugPortVersion::DPv2 {
            let target_id = TARGETID(self.targetid);
            let dlpidr = DLPIDR(self.dlpidr);

            let part_no = target_id.tpartno();
            let revision = target_id.trevision();

            let designer_id = target_id.tdesigner();

            let cc = (designer_id >> 7) as u8;
            let id = (designer_id & 0x7f) as u8;

            let designer = jep106::JEP106Code::new(cc, id);

            format_jep(f, designer)?;
            write!(f, ", Part: {part_no:#x}")?;
            write!(f, ", Revision: {revision:#x}")?;

            let instance = dlpidr.tinstance();

            write!(f, ", Instance: {instance:#04x}")?;
        } else {
            format_jep(f, self.dp_info.designer.into())?;
        }

        Ok(())
    }
}

impl Display for DebugPortInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut tree = Tree::new(self.dp_info.to_string());
        if self.aps.is_empty() {
            tree.push(Tree::new("No access ports found on this chip.".to_string()));
        } else {
            for ap in &self.aps {
                match ap {
                    ApInfo::MemoryAp {
                        ap_addr,
                        component_tree,
                    } => {
                        let mut ap_root = Tree::new(format!("{} MemoryAP", ap_addr.ap));

                        ap_root.push(component_tree);

                        tree.push(ap_root);
                    }
                    ApInfo::ApV2Root { component_tree } => {
                        for child in component_tree.children.iter() {
                            tree.push(child);
                        }
                    }
                    ApInfo::Unknown { ap_addr, idr } => {
                        let idr = IDR::try_from(*idr).unwrap();
                        let jep = idr.DESIGNER;

                        let ap_type = if idr.DESIGNER == JEP_ARM {
                            format!("{:?}", idr.TYPE)
                        } else {
                            format!("{:#x}", idr.TYPE as u8)
                        };

                        let ap_node = Tree::new(format!(
                            "{} Unknown AP (Designer: {}, Class: {:?}, Type: {}, Variant: {:#x}, Revision: {:#x})",
                            ap_addr.ap,
                            jep.get().unwrap_or("<unknown>"),
                            idr.CLASS,
                            ap_type,
                            idr.VARIANT,
                            idr.REVISION
                        ));

                        tree.push(ap_node);
                    }
                };
            }
        }

        write!(f, "{tree}")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn jep_arm_is_arm() {
        assert_eq!(super::JEP_ARM.get(), Some("ARM Ltd"))
    }
}
