use std::fmt::Display;

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

fn component_tree_to_json(node: &ComponentTreeNode) -> serde_json::Value {
    let mut obj = serde_json::json!({
        "node": node.node,
    });
    if let Some(addr) = node.address {
        obj["address"] = serde_json::json!(format!("{addr:#010x}"));
    }
    if let Some(kind) = &node.kind {
        obj["kind"] = serde_json::json!(kind);
    }
    if !node.children.is_empty() {
        obj["children"] = node.children.iter().map(component_tree_to_json).collect();
    }
    obj
}

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
    /// Output as JSON for programmatic consumption
    #[arg(long)]
    json: bool,
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
    pub async fn run(self, client: RpcClient) -> anyhow::Result<()> {
        let protocols = if let Some(protocol) = self.common.protocol {
            vec![protocol]
        } else {
            vec![WireProtocol::Jtag, WireProtocol::Swd]
        };

        let probe = select_probe(&client, self.common.probe.clone().map(Into::into)).await?;

        if self.json {
            return self.run_json(client, probe, protocols).await;
        }

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
                println!("Error while probing target: {}", error);
            }

            if successes.is_empty() {
                for message in errors {
                    println!("{}", message);
                }
            } else {
                for message in successes {
                    println!("{}", message);
                }
            }
        }

        Ok(())
    }

    async fn run_json(
        self,
        client: RpcClient,
        probe: crate::rpc::functions::probe::DebugProbeEntry,
        protocols: Vec<WireProtocol>,
    ) -> anyhow::Result<()> {
        let mut results: Vec<serde_json::Value> = vec![];

        for protocol in protocols {
            let req = TargetInfoRequest {
                target_sel: self.target_sel,
                protocol: protocol.into(),
                probe: probe.clone(),
                speed: self.common.speed,
                connect_under_reset: self.common.connect_under_reset,
                dry_run: self.common.dry_run,
            };

            let mut arm_dps: Vec<serde_json::Value> = vec![];
            let mut idcodes: Vec<serde_json::Value> = vec![];

            let _ = client
                .info(req, async |message| match message {
                    InfoEvent::ArmDp(dp_info) => {
                        arm_dps.push(dp_info_to_json(&dp_info));
                    }
                    InfoEvent::Idcode {
                        architecture,
                        idcode: Some(idcode),
                    } => {
                        idcodes.push(serde_json::json!({
                            "arch": architecture,
                            "idcode": format!("{idcode:#010x}"),
                        }));
                    }
                    _ => {}
                })
                .await;

            if !arm_dps.is_empty() || !idcodes.is_empty() {
                let mut entry = serde_json::json!({ "protocol": protocol.to_string() });
                if !arm_dps.is_empty() {
                    entry["arm_dps"] = serde_json::Value::Array(arm_dps);
                }
                if !idcodes.is_empty() {
                    entry["idcodes"] = serde_json::Value::Array(idcodes);
                }
                results.push(entry);
            }
        }

        println!("{}", serde_json::to_string(&results)?);
        Ok(())
    }
}

fn dp_info_to_json(dp: &DebugPortInfo) -> serde_json::Value {
    let dp_version = match dp.dp_info.dp_info.version {
        DebugPortVersion::DPv0 => "DPv0",
        DebugPortVersion::DPv1 => "DPv1",
        DebugPortVersion::DPv2 => "DPv2",
        DebugPortVersion::DPv3 => "DPv3",
        DebugPortVersion::Unsupported(_) => "Unsupported",
    };
    let mindp = dp.dp_info.dp_info.min_dp_support == MinDpSupport::Implemented;
    let designer: jep106::JEP106Code = dp.dp_info.dp_info.designer.into();
    let designer_name = designer.get().unwrap_or("<unknown>").to_string();

    let aps: Vec<serde_json::Value> = dp
        .aps
        .iter()
        .map(|ap| match ap {
            ApInfo::MemoryAp {
                ap_addr,
                component_tree,
            } => serde_json::json!({
                "type": "MemoryAP",
                "ap": ap_addr.ap,
                "tree": component_tree_to_json(component_tree),
            }),
            ApInfo::ApV2Root { component_tree } => serde_json::json!({
                "type": "ApV2Root",
                "children": component_tree.children.iter().map(component_tree_to_json).collect::<Vec<_>>(),
            }),
            ApInfo::Unknown { ap_addr, idr } => serde_json::json!({
                "type": "Unknown",
                "ap": ap_addr.ap,
                "idr": format!("{idr:#010x}"),
            }),
        })
        .collect();

    serde_json::json!({
        "dp": dp_version,
        "mindp": mindp,
        "designer": designer_name,
        "aps": aps,
    })
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

            write!(f, ", Instance: {:#04x}", instance)?;
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
