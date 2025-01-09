//! Read information about the connected target using the selected wire protocol.
//!
//! The information is passed as a stream of messages to the provided emitter.

use anyhow::anyhow;
use postcard_rpc::header::{VarHeader, VarSeq};
use postcard_schema::{schema, Schema};
use probe_rs::{
    architecture::{
        arm::{
            self,
            ap::ApClass,
            armv6m::Demcr,
            component::Scs,
            dp::{self, Ctrl, DLPIDR, DPIDR, TARGETID},
            memory::{
                romtable::{PeripheralID, RomTable},
                Component, ComponentId, CoresightComponent, PeripheralType,
            },
            sequences::DefaultArmSequence,
            ArmProbeInterface, Register,
        },
        riscv::communication_interface::RiscvCommunicationInterface,
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState,
        },
    },
    probe::{Probe, WireProtocol as ProbeRsWireProtocol},
    MemoryMappedRegister,
};
use serde::{Deserialize, Serialize};

use crate::{
    rpc::functions::{
        chip::JEP106Code,
        probe::{DebugProbeEntry, WireProtocol},
        NoResponse, RpcContext, TargetInfoDataTopic,
    },
    util::common_options::ProbeOptions,
};

#[derive(Serialize, Deserialize, Schema)]
pub struct TargetInfoRequest {
    pub probe: DebugProbeEntry,
    pub speed: Option<u32>,
    pub connect_under_reset: bool,
    pub dry_run: bool,
    pub target_sel: Option<u32>,
    pub protocol: WireProtocol,
}

impl From<&TargetInfoRequest> for ProbeOptions {
    fn from(request: &TargetInfoRequest) -> Self {
        ProbeOptions {
            chip: None,
            chip_description_path: None,
            protocol: match request.protocol {
                WireProtocol::Jtag => Some(ProbeRsWireProtocol::Jtag),
                WireProtocol::Swd => Some(ProbeRsWireProtocol::Swd),
            },
            non_interactive: true,
            probe: Some(request.probe.selector().into()),
            speed: request.speed,
            connect_under_reset: request.connect_under_reset,
            dry_run: request.dry_run,
            allow_erase_all: false,
        }
    }
}

pub async fn target_info(
    ctx: &mut RpcContext,
    _hdr: VarHeader,
    request: TargetInfoRequest,
) -> NoResponse {
    let probe_options = ProbeOptions::from(&request).load()?;

    let probe = probe_options.attach_probe(&ctx.lister())?;

    if let Err(e) = try_show_info(
        ctx,
        probe,
        request.protocol,
        probe_options.connect_under_reset(),
        request.target_sel,
    )
    .await
    {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::Message(format!(
                "Failed to identify target using protocol {}: {e:?}",
                request.protocol
            )),
        )
        .await?;
    }

    Ok(())
}

#[derive(Clone, Serialize, Deserialize, Schema)]
pub enum InfoEvent {
    Message(String),
    ProtocolNotSupportedByArch {
        architecture: String,
        protocol: WireProtocol,
    },
    ProbeInterfaceMissing {
        interface: String,
        architecture: String,
    },
    Error {
        architecture: String,
        error: String,
    },
    ArmError {
        dp_addr: DpAddress,
        error: String,
    },
    Idcode {
        architecture: String,
        idcode: Option<u32>,
    },
    ArmDp(DebugPortInfo),
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash, Serialize, Deserialize, Schema)]
pub enum DpAddress {
    /// Access the single DP on the bus, assuming there is only one.
    /// Will cause corruption if multiple are present.
    Default,
    /// Select a particular DP on a SWDv2 multidrop bus. The contained `u32` is
    /// the `TARGETSEL` value to select it.
    Multidrop(u32),
}

impl From<arm::DpAddress> for DpAddress {
    fn from(address: arm::DpAddress) -> Self {
        match address {
            arm::DpAddress::Default => DpAddress::Default,
            arm::DpAddress::Multidrop(target_sel) => DpAddress::Multidrop(target_sel),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct DebugPortInfoNode {
    pub dp_info: DebugPortId,
    pub targetid: u32,
    pub dlpidr: u32,
}

/// The ID of a debug port. Can be used to detect and select devices in a multidrop setup.
#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct DebugPortId {
    /// The revision of the debug port (implementation defined). This is what the designer of the debug port chooses.
    pub revision: u8,
    /// The part number of the debug port (determined by the designer).
    pub part_no: u8,
    /// The version of this debug port. This is what the selected spec says.
    pub version: DebugPortVersion,
    /// Specifies if pushed-find operations are implemented or not.
    pub min_dp_support: MinDpSupport,
    /// The JEP106 code of the designer of this debug port.
    pub designer: JEP106Code,
}

impl From<dp::DebugPortId> for DebugPortId {
    fn from(id: dp::DebugPortId) -> Self {
        Self {
            revision: id.revision,
            part_no: id.part_no,
            version: id.version.into(),
            min_dp_support: id.min_dp_support.into(),
            designer: id.designer.into(),
        }
    }
}

/// The version of the debug port.
#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize, Schema)]
pub enum DebugPortVersion {
    /// Version 0 (not common)
    DPv0,
    /// Version 1 (most of the ARM cores feature this version)
    DPv1,
    /// Version 2 (**very** rare (only known example is the RP2040))
    DPv2,
    /// Version 3 (on ADIv6 devices)
    DPv3,
    /// Some unsupported value was encountered!
    Unsupported(u8),
}

impl From<dp::DebugPortVersion> for DebugPortVersion {
    fn from(version: dp::DebugPortVersion) -> Self {
        match version {
            dp::DebugPortVersion::DPv0 => DebugPortVersion::DPv0,
            dp::DebugPortVersion::DPv1 => DebugPortVersion::DPv1,
            dp::DebugPortVersion::DPv2 => DebugPortVersion::DPv2,
            dp::DebugPortVersion::DPv3 => DebugPortVersion::DPv3,
            dp::DebugPortVersion::Unsupported(v) => DebugPortVersion::Unsupported(v),
        }
    }
}

/// Specifies if pushed-find operations are implemented or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum MinDpSupport {
    /// Pushed-find operations are **not** implemented.
    NotImplemented,
    /// Pushed-find operations are implemented.
    Implemented,
}

impl From<dp::MinDpSupport> for MinDpSupport {
    fn from(support: dp::MinDpSupport) -> Self {
        match support {
            dp::MinDpSupport::NotImplemented => MinDpSupport::NotImplemented,
            dp::MinDpSupport::Implemented => MinDpSupport::Implemented,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub struct DebugPortInfo {
    pub dp_info: DebugPortInfoNode,
    pub aps: Vec<ApInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Schema)]
pub enum ApInfo {
    MemoryAp {
        ap_addr: FullyQualifiedApAddress,
        component_tree: ComponentTreeNode,
    },
    Unknown {
        ap_addr: FullyQualifiedApAddress,
        idr: u32,
    },
}

/// Access port address.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Hash, Serialize, Deserialize, Schema)]
pub struct FullyQualifiedApAddress {
    /// The address of the debug port this access port belongs to.
    pub dp: DpAddress,
    /// The access port number.
    pub ap: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComponentTreeNode {
    pub node: String,
    pub children: Vec<ComponentTreeNode>,
}

impl postcard_schema::Schema for ComponentTreeNode {
    const SCHEMA: &'static schema::NamedType = &schema::NamedType {
        name: "ComponentTreeNode",
        ty: &schema::DataModelType::Struct(&[
            &schema::NamedValue {
                name: "node",
                ty: <String as ::postcard_schema::Schema>::SCHEMA,
            },
            &schema::NamedValue {
                name: "children",
                ty: <Vec<()> as ::postcard_schema::Schema>::SCHEMA,
            },
        ]),
    };
}

impl From<String> for ComponentTreeNode {
    fn from(node: String) -> Self {
        Self::new(node)
    }
}

impl ComponentTreeNode {
    fn new(node: String) -> Self {
        Self {
            node,
            children: vec![],
        }
    }

    fn push(&mut self, child: impl Into<ComponentTreeNode>) {
        self.children.push(child.into());
    }
}

async fn try_show_info(
    ctx: &mut RpcContext,
    mut probe: Probe,
    protocol: WireProtocol,
    connect_under_reset: bool,
    target_sel: Option<u32>,
) -> anyhow::Result<()> {
    probe.select_protocol(ProbeRsWireProtocol::from(protocol))?;

    if connect_under_reset {
        probe.attach_to_unspecified_under_reset()?;
    } else {
        probe.attach_to_unspecified()?;
    }

    if probe.has_arm_interface() {
        let dp_addr = if let Some(target_sel) = target_sel {
            vec![arm::DpAddress::Multidrop(target_sel)]
        } else {
            vec![
                arm::DpAddress::Default,
                // RP2040
                arm::DpAddress::Multidrop(0x01002927),
                arm::DpAddress::Multidrop(0x11002927),
            ]
        };

        for address in dp_addr {
            match try_show_arm_dp_info(ctx, probe, address).await {
                (probe_moved, Ok(dp_version)) => {
                    probe = probe_moved;
                    if dp_version < dp::DebugPortVersion::DPv2 && target_sel.is_none() {
                        let message = format!("Debug port version {dp_version} does not support SWD multidrop. Stopping here.");

                        ctx.publish::<TargetInfoDataTopic>(
                            VarSeq::Seq2(0),
                            &InfoEvent::Message(message),
                        )
                        .await?;
                        break;
                    }
                }
                (probe_moved, Err(e)) => {
                    probe = probe_moved;

                    ctx.publish::<TargetInfoDataTopic>(
                        VarSeq::Seq2(0),
                        &InfoEvent::ArmError {
                            dp_addr: address.into(),
                            error: format!("{e:?}"),
                        },
                    )
                    .await?;
                }
            }
        }
    } else {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProbeInterfaceMissing {
                interface: "DAP".to_string(),
                architecture: "ARM".to_string(),
            },
        )
        .await?;
    }

    if let Err(error) = try_read_riscv_info(ctx, &mut probe, protocol).await {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::Error {
                architecture: "RISC-V".to_string(),
                error: format!("{error:?}"),
            },
        )
        .await?;
    }

    if let Err(error) = try_read_xtensa_info(ctx, &mut probe, protocol).await {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::Error {
                architecture: "Xtensa".to_string(),
                error: format!("{error:?}"),
            },
        )
        .await?;
    }

    Ok(())
}

async fn try_read_riscv_info(
    ctx: &mut RpcContext,
    probe: &mut Probe,
    protocol: WireProtocol,
) -> Result<(), anyhow::Error> {
    if probe.has_riscv_interface() && protocol == WireProtocol::Jtag {
        tracing::debug!("Trying to show RISC-V chip information");
        let factory = probe.try_get_riscv_interface_builder()?;

        let mut state = factory.create_state();
        let mut interface = factory.attach(&mut state)?;
        show_riscv_info(ctx, &mut interface).await?;
    } else if protocol == WireProtocol::Swd {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProtocolNotSupportedByArch {
                architecture: "RISC-V".to_string(),
                protocol,
            },
        )
        .await?;
    } else {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProbeInterfaceMissing {
                interface: "RISC-V".to_string(),
                architecture: "RISC-V".to_string(),
            },
        )
        .await?;
    }

    Ok(())
}

async fn try_read_xtensa_info(
    ctx: &mut RpcContext,
    probe: &mut Probe,
    protocol: WireProtocol,
) -> Result<(), anyhow::Error> {
    if probe.has_xtensa_interface() && protocol == WireProtocol::Jtag {
        tracing::debug!("Trying to show Xtensa chip information");
        let mut state = XtensaDebugInterfaceState::default();
        let mut interface = probe.try_get_xtensa_interface(&mut state)?;

        show_xtensa_info(ctx, &mut interface).await?;
    } else if protocol == WireProtocol::Swd {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProtocolNotSupportedByArch {
                architecture: "Xtensa".to_string(),
                protocol,
            },
        )
        .await?;
    } else {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProbeInterfaceMissing {
                interface: "Xtensa".to_string(),
                architecture: "Xtensa".to_string(),
            },
        )
        .await?;
    }

    Ok(())
}

async fn try_show_arm_dp_info(
    ctx: &mut RpcContext,
    probe: Probe,
    dp_address: arm::DpAddress,
) -> (Probe, anyhow::Result<dp::DebugPortVersion>) {
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
            let res = show_arm_info(ctx, &mut *interface, dp_address).await;
            (interface.close(), res)
        }
        Err((probe, e)) => (probe, Err(e)),
    }
}

/// Try to show information about the ARM chip, connected to a DP at the given address.
///
/// Returns the version of the DP.
async fn show_arm_info(
    ctx: &mut RpcContext,
    interface: &mut dyn ArmProbeInterface,
    dp: arm::DpAddress,
) -> anyhow::Result<dp::DebugPortVersion> {
    let dp_info = interface.read_raw_dp_register(dp, DPIDR::ADDRESS)?;
    let dp_info = dp::DebugPortId::from(DPIDR(dp_info));

    let dpinfo = if dp_info.version == dp::DebugPortVersion::DPv2 {
        let targetid = interface.read_raw_dp_register(dp, TARGETID::ADDRESS)?;

        // Read Instance ID
        let dlpidr = interface.read_raw_dp_register(dp, DLPIDR::ADDRESS)?;

        // Read from the CTRL/STAT register, to ensure that the dpbanksel field is set to zero.
        // This helps with error handling later, because it means the CTRL/AP register can be
        // read in case of an error.
        let _ = interface.read_raw_dp_register(dp, Ctrl::ADDRESS)?;

        DebugPortInfoNode {
            dp_info: dp_info.into(),
            targetid,
            dlpidr,
        }
    } else {
        DebugPortInfoNode {
            dp_info: dp_info.into(),
            targetid: 0,
            dlpidr: 0,
        }
    };

    let mut info = DebugPortInfo {
        dp_info: dpinfo.clone(),
        aps: vec![],
    };

    let access_ports = interface.access_ports(dp)?;

    ctx.publish::<TargetInfoDataTopic>(
        VarSeq::Seq2(0),
        &InfoEvent::Message(format!("ARM Chip with debug port {:x?}:", dp)),
    )
    .await?;

    for ap_address in access_ports {
        use probe_rs::architecture::arm::ap::IDR;
        let raw_idr = interface.read_raw_ap_register(&ap_address, IDR::ADDRESS)?;
        let idr: IDR = raw_idr.try_into()?;

        let ap_info = if idr.CLASS == ApClass::MemAp {
            let component_tree = match handle_memory_ap(interface, &ap_address) {
                Ok(component_tree) => component_tree,
                Err(e) => ComponentTreeNode::new(format!("Error during access: {e}")),
            };
            ApInfo::MemoryAp {
                ap_addr: FullyQualifiedApAddress {
                    dp: ap_address.dp().into(),
                    ap: ap_address.ap().to_string(),
                },
                component_tree,
            }
        } else {
            ApInfo::Unknown {
                ap_addr: FullyQualifiedApAddress {
                    dp: ap_address.dp().into(),
                    ap: ap_address.ap().to_string(),
                },
                idr: raw_idr,
            }
        };

        info.aps.push(ap_info);
    }

    ctx.publish::<TargetInfoDataTopic>(VarSeq::Seq2(0), &InfoEvent::ArmDp(info))
        .await?;

    Ok(dp_info.version)
}

fn handle_memory_ap(
    interface: &mut dyn ArmProbeInterface,
    access_port: &arm::FullyQualifiedApAddress,
) -> anyhow::Result<ComponentTreeNode> {
    let component = {
        let mut memory = interface.memory_interface(access_port)?;

        // Check if the AP is accessible
        let csw = memory.generic_status()?;
        if !csw.DeviceEn {
            return Ok(ComponentTreeNode::new(
                "Memory AP is not accessible, DeviceEn bit not set".to_string(),
            ));
        }

        let base_address = memory.base_address()?;
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
    access_port: &arm::FullyQualifiedApAddress,
) -> anyhow::Result<ComponentTreeNode> {
    let tree = match &component {
        Component::GenericVerificationComponent(_) => ComponentTreeNode::new("Generic".to_string()),
        Component::Class1RomTable(id, table) => {
            let peripheral_id = id.peripheral_id();

            let root = if let Some(part) = peripheral_id.determine_part() {
                format!("{} (ROM Table, Class 1)", part.name())
            } else {
                match peripheral_id.designer() {
                    Some(designer) => format!("ROM Table (Class 1), Designer: {designer}"),
                    None => "ROM Table (Class 1)".to_string(),
                }
            };

            let mut tree = ComponentTreeNode::new(root);
            process_vendor_rom_tables(interface, id, table, access_port, &mut tree)?;

            for entry in table.entries() {
                let component = entry.component().clone();

                tree.push(coresight_component_tree(interface, component, access_port)?);
            }

            tree
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
                    peripheral_id.designer()
                        .unwrap_or("<unknown>"),
                )
            };

            let mut tree = ComponentTreeNode::new(component_description);
            process_component_entry(&mut tree, interface, peripheral_id, &component, access_port)?;

            tree
        }

        Component::PeripheralTestBlock(_) => {
            ComponentTreeNode::new("Peripheral test block".to_string())
        }
        Component::GenericIPComponent(id) => {
            let peripheral_id = id.peripheral_id();

            let desc = if let Some(part_desc) = peripheral_id.determine_part() {
                format!("{: <15} (Generic IP component)", part_desc.name())
            } else {
                "Generic IP component".to_string()
            };

            let mut tree = ComponentTreeNode::new(desc);
            process_component_entry(&mut tree, interface, peripheral_id, &component, access_port)?;

            tree
        }

        Component::CoreLinkOrPrimeCellOrSystemComponent(_) => {
            ComponentTreeNode::new("Core Link / Prime Cell / System component".to_string())
        }
    };

    Ok(tree)
}

/// Processes information from/around manufacturer-specific ROM tables and adds them to the tree.
///
/// Some manufacturer-specific ROM tables contain more than just entries. This function tries
/// to make sense of these tables.
fn process_vendor_rom_tables(
    interface: &mut dyn ArmProbeInterface,
    id: &ComponentId,
    _table: &RomTable,
    access_port: &arm::FullyQualifiedApAddress,
    tree: &mut ComponentTreeNode,
) -> anyhow::Result<()> {
    let peripheral_id = id.peripheral_id();
    let Some(part_info) = peripheral_id.determine_part() else {
        return Ok(());
    };

    if part_info.peripheral_type() == PeripheralType::Custom && part_info.name() == "Atmel DSU" {
        use probe_rs::vendor::microchip::sequences::atsam::DsuDid;

        // Read and parse the DID register
        let did = DsuDid(
            interface
                .memory_interface(access_port)?
                .read_word_32(DsuDid::ADDRESS)?,
        );

        tree.push(format!("Atmel device (DID = {:#010x})", did.0));
    }

    Ok(())
}

/// Processes ROM table entries and adds them to the tree.
fn process_component_entry(
    tree: &mut ComponentTreeNode,
    interface: &mut dyn ArmProbeInterface,
    peripheral_id: &PeripheralID,
    component: &Component,
    access_port: &arm::FullyQualifiedApAddress,
) -> anyhow::Result<()> {
    let Some(part) = peripheral_id.determine_part() else {
        return Ok(());
    };

    if part.peripheral_type() == PeripheralType::Scs {
        let cc = &CoresightComponent::new(component.clone(), access_port.clone());
        let scs = &mut Scs::new(interface, cc);
        let cpu_tree = cpu_info_tree(scs)?;

        tree.push(cpu_tree);
    }

    Ok(())
}

fn cpu_info_tree(scs: &mut Scs) -> anyhow::Result<ComponentTreeNode> {
    let mut tree = ComponentTreeNode::new("CPUID".into());

    let cpuid = scs.cpuid()?;

    tree.push(format!("IMPLEMENTER: {}", cpuid.implementer_name()));
    tree.push(format!("VARIANT: {}", cpuid.variant()));
    tree.push(format!("PARTNO: {}", cpuid.part_name()));
    tree.push(format!("REVISION: {}", cpuid.revision()));

    Ok(tree)
}

async fn show_riscv_info(
    ctx: &mut RpcContext,
    interface: &mut RiscvCommunicationInterface<'_>,
) -> anyhow::Result<()> {
    let idcode = interface.read_idcode()?;

    ctx.publish::<TargetInfoDataTopic>(
        VarSeq::Seq2(0),
        &InfoEvent::Idcode {
            architecture: "RISC-V".to_string(),
            idcode,
        },
    )
    .await
}

async fn show_xtensa_info(
    ctx: &mut RpcContext,
    interface: &mut XtensaCommunicationInterface<'_>,
) -> anyhow::Result<()> {
    let idcode = interface.read_idcode()?;

    ctx.publish::<TargetInfoDataTopic>(
        VarSeq::Seq2(0),
        &InfoEvent::Idcode {
            architecture: "Xtensa".to_string(),
            idcode: Some(idcode),
        },
    )
    .await
}
