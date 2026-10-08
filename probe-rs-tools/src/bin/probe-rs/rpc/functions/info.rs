//! Read information about the connected target using the selected wire protocol.
//!
//! The information is passed as a stream of messages to the provided emitter.

use anyhow::anyhow;
use jep106::JEP106Code;
use postcard_rpc::header::{VarHeader, VarSeq};
use probe_rs::{
    MemoryMappedRegister as _,
    architecture::{
        arm::{
            self, ApAddress, ApV2Address, ArmDebugInterface,
            ap::{ApClass, ApRegister, ApType, IDR},
            armv6m::Demcr,
            component::Scs,
            dp::{self, Ctrl, DLPIDR, DPIDR, DpRegister, TARGETID},
            memory::{
                ArmMemoryInterface, Component, ComponentId, CoresightComponent, PeripheralType,
                romtable::{PeripheralID, RomTable},
            },
            sequences::DefaultArmSequence,
        },
        riscv::{
            Dmcontrol,
            communication_interface::{
                DebugModuleVersion, RiscvCommunicationInterface, RiscvError,
            },
        },
        xtensa::communication_interface::{
            XtensaCommunicationInterface, XtensaDebugInterfaceState,
        },
    },
    probe::{Probe, wlink::WchLink},
};
use probe_rs_rpc::info::{
    ApInfo, ComponentTreeNode, DebugPortId, DebugPortInfo, DebugPortInfoNode, DebugPortVersion,
    DpAddress, FullyQualifiedApAddress, InfoEvent, JtagTapInfo, MinDpSupport, RiscvDebugModuleInfo,
    RiscvDebugModuleVersion, RiscvHartInfo, RiscvHartIsa, TargetInfoRequest, TargetMetadataRequest,
    WireFlashSector, WireSessionCore, WireSessionTargetMetadata,
};
use probe_rs_rpc::{NoResponse, TargetInfoDataTopic, probe::WireProtocol};
use probe_rs_target::ScanChainElement;

use crate::rpc::functions::chip::convert::to_wire_memory_region;
use crate::rpc::functions::core_ops::convert::to_wire_core_type;
use crate::rpc::functions::probe::convert::from_wire_protocol;
use crate::{
    rpc::functions::{RpcContext, convert::lift},
    util::common_options::ProbeOptions,
};

pub async fn target_metadata(
    ctx: &mut RpcContext,
    _hdr: VarHeader,
    request: TargetMetadataRequest,
) -> probe_rs_rpc::info::TargetMetadataResponse {
    let session = ctx.session(request.sessid).await;
    let target = session.target();
    Ok(WireSessionTargetMetadata {
        target_name: target.name.clone(),
        default_format: target.default_format.clone(),
        cores: session
            .list_cores()
            .into_iter()
            .map(|(index, core_type)| WireSessionCore {
                index: index as u32,
                core_type: to_wire_core_type(core_type),
            })
            .collect(),
        memory_map: target
            .memory_map
            .iter()
            .cloned()
            .map(to_wire_memory_region)
            .collect(),
        flash_sectors: wire_flash_sectors(target),
    })
}

/// Deduplicated absolute flash sectors for GDB memory-map XML.
fn wire_flash_sectors(target: &probe_rs::Target) -> Vec<WireFlashSector> {
    use std::collections::BTreeMap;
    use std::collections::btree_map::Entry;

    let mut regions = BTreeMap::new();
    for algo in target.flash_algorithms.iter() {
        let start = algo.flash_properties.address_range.start;
        let end = if let Some(region) =
            target.memory_region_by_address(algo.flash_properties.address_range.start)
        {
            region.address_range().end
        } else {
            algo.flash_properties.address_range.end
        };
        let mut sectors = algo.flash_properties.sectors.clone();
        sectors.sort_by_key(|s| s.address);
        sectors.push(probe_rs::config::SectorDescription {
            size: 0,
            address: end - start,
        });
        for (current, next) in sectors.iter().zip(sectors.iter().skip(1)) {
            let sector = WireFlashSector {
                start: start + current.address,
                length: next.address - current.address,
                blocksize: current.size,
            };
            match regions.entry(sector.start) {
                Entry::Vacant(e) => {
                    e.insert(sector);
                }
                Entry::Occupied(mut e) => {
                    if sector.blocksize < e.get().blocksize {
                        e.insert(sector);
                    }
                }
            }
        }
    }
    regions.into_values().collect()
}

pub async fn target_info(
    ctx: &mut RpcContext,
    _hdr: VarHeader,
    request: TargetInfoRequest,
) -> NoResponse {
    let mut registry = ctx.registry().await;
    let probe_options = ProbeOptions::from(&request).load(&mut registry)?;

    let probe = probe_options.attach_probe(&ctx.lister())?;

    if let Err(e) = try_show_info(
        ctx,
        probe,
        request.scan_chain.clone(),
        request.protocol,
        probe_options.connect_under_reset(),
        request.target_sel,
    )
    .await
    {
        lift(
            ctx.publish::<TargetInfoDataTopic>(
                VarSeq::Seq2(0),
                &InfoEvent::Message(format!(
                    "Failed to identify target using protocol {}: {e:?}",
                    request.protocol
                )),
            )
            .await,
        )?;
    }

    Ok(())
}

async fn try_show_info(
    ctx: &mut RpcContext,
    mut probe: Probe,
    scan_chain: Vec<u8>,
    protocol: WireProtocol,
    connect_under_reset: bool,
    target_sel: Option<u32>,
) -> anyhow::Result<()> {
    probe.select_protocol(from_wire_protocol(protocol))?;

    if !scan_chain.is_empty()
        && let Some(mut chain) = probe.try_as_jtag_chain()
    {
        let chain_elements = scan_chain
            .iter()
            .map(|&ir_len| ScanChainElement {
                name: None,
                ir_len: Some(ir_len),
            })
            .collect::<Vec<_>>();
        chain.set_chain(&chain_elements);
    }

    if connect_under_reset {
        probe.attach_to_unspecified_under_reset()?;
    } else {
        probe.attach_to_unspecified()?;
    }

    if protocol == WireProtocol::Jtag
        && let Some(taps) = read_jtag_scan_chain(&mut probe)
    {
        return show_jtag_scan_chain_info(ctx, probe, taps).await;
    }

    if probe.has_arm_debug_interface() {
        let dp_addr = if let Some(target_sel) = target_sel {
            vec![dp::DpAddress::Multidrop(target_sel)]
        } else {
            vec![
                dp::DpAddress::Default,
                // RP2040
                dp::DpAddress::Multidrop(0x01002927),
                dp::DpAddress::Multidrop(0x11002927),
            ]
        };

        for address in dp_addr {
            match try_show_arm_dp_info(ctx, probe, address, true).await {
                (probe_moved, Ok(dp_version)) => {
                    probe = probe_moved;
                    if dp_version < dp::DebugPortVersion::DPv2 && target_sel.is_none() {
                        let message = format!(
                            "Debug port version {dp_version} does not support SWD multidrop. Stopping here."
                        );

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
                            dp_addr: convert::to_wire_dp_address(address),
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

/// Returns the TAPs of the JTAG scan chain, or `None` when the probe cannot see the chain.
fn read_jtag_scan_chain(probe: &mut Probe) -> Option<Vec<JtagTapInfo>> {
    let mut chain = probe.try_as_jtag_chain()?;

    let ir_lens = match chain.scan_chain() {
        Ok(elements) if !elements.is_empty() => {
            elements.iter().map(|e| e.ir_len()).collect::<Vec<_>>()
        }
        Ok(_) => return None,
        Err(error) => {
            tracing::debug!("Unable to scan the JTAG chain: {error}");
            return None;
        }
    };

    // A chain that was set without a scan has no IDCODEs.
    let idcodes = chain
        .idcodes()
        .iter()
        .copied()
        .chain(std::iter::repeat(None));

    Some(
        ir_lens
            .into_iter()
            .zip(idcodes)
            .map(|(ir_len, idcode)| JtagTapInfo { idcode, ir_len })
            .collect(),
    )
}

/// The debug interfaces that a TAP can have.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapKind {
    Arm,
    Riscv,
    Xtensa,
    RiscvOrXtensa,
    Unsupported,
}

impl TapKind {
    fn classify(tap: &JtagTapInfo) -> Self {
        let designer = tap.idcode.map(idcode_designer);

        // ADIv5 and ADIv6 set the IR length of a JTAG-DP to 4. The RISC-V and Xtensa TAPs that
        // probe-rs supports have an IR length of 5. The RISC-V DTM instructions do other things
        // on other TAPs, for example they start the configuration of a Xilinx FPGA.
        match (tap.ir_len, designer) {
            (_, Some(JEP_ST)) => Self::Unsupported,
            (4, _) => Self::Arm,
            (5, Some(JEP_TENSILICA)) => Self::Xtensa,
            (5, Some(_)) => Self::Riscv,
            (5, None) => Self::RiscvOrXtensa,
            _ => Self::Unsupported,
        }
    }
}

/// STM32 and GD32 devices put a boundary-scan TAP with this designer next to the JTAG-DP.
const JEP_ST: JEP106Code = JEP106Code::new(0, 0x20);
const JEP_TENSILICA: JEP106Code = JEP106Code::new(4, 0x72);

fn idcode_designer(idcode: u32) -> JEP106Code {
    let manufacturer = (idcode >> 1) & 0x7ff;
    JEP106Code::new((manufacturer >> 7) as u8, (manufacturer & 0x7f) as u8)
}

async fn show_jtag_scan_chain_info(
    ctx: &mut RpcContext,
    mut probe: Probe,
    taps: Vec<JtagTapInfo>,
) -> anyhow::Result<()> {
    ctx.publish::<TargetInfoDataTopic>(VarSeq::Seq2(0), &InfoEvent::JtagScanChain(taps.clone()))
        .await?;

    for (index, tap) in taps.iter().enumerate() {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::JtagTap {
                index: index as u32,
            },
        )
        .await?;

        if let Some(mut chain) = probe.try_as_jtag_chain()
            && let Err(error) = chain.select(index)
        {
            ctx.publish::<TargetInfoDataTopic>(
                VarSeq::Seq2(0),
                &InfoEvent::Message(format!("Unable to select the TAP: {error}")),
            )
            .await?;
            continue;
        }

        match TapKind::classify(tap) {
            TapKind::Arm => probe = show_arm_tap_info(ctx, probe).await?,
            TapKind::Riscv => {
                if !show_riscv_tap_info(ctx, &mut probe).await? {
                    show_unsupported_tap(ctx).await?;
                }
            }
            TapKind::Xtensa => show_xtensa_tap_info(ctx, &mut probe).await?,
            TapKind::RiscvOrXtensa => {
                if !show_riscv_tap_info(ctx, &mut probe).await? {
                    show_xtensa_tap_info(ctx, &mut probe).await?;
                }
            }
            TapKind::Unsupported => show_unsupported_tap(ctx).await?,
        }
    }

    Ok(())
}

async fn show_unsupported_tap(ctx: &mut RpcContext) -> anyhow::Result<()> {
    ctx.publish::<TargetInfoDataTopic>(
        VarSeq::Seq2(0),
        &InfoEvent::Message("No supported debug interface.".to_string()),
    )
    .await?;
    Ok(())
}

async fn show_arm_tap_info(ctx: &mut RpcContext, probe: Probe) -> anyhow::Result<Probe> {
    if !probe.has_arm_debug_interface() {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProbeInterfaceMissing {
                interface: "DAP".to_string(),
                architecture: "ARM".to_string(),
            },
        )
        .await?;
        return Ok(probe);
    }

    let address = dp::DpAddress::Default;
    let (probe, result) = try_show_arm_dp_info(ctx, probe, address, false).await;
    if let Err(e) = result {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ArmError {
                dp_addr: convert::to_wire_dp_address(address),
                error: format!("{e:?}"),
            },
        )
        .await?;
    }

    Ok(probe)
}

/// Shows the RISC-V debug module of the selected TAP. Returns `false` when the TAP is not a RISC-V
/// DTM, and shows nothing then.
async fn show_riscv_tap_info(ctx: &mut RpcContext, probe: &mut Probe) -> anyhow::Result<bool> {
    if !probe.has_riscv_interface() {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::ProbeInterfaceMissing {
                interface: "RISC-V".to_string(),
                architecture: "RISC-V".to_string(),
            },
        )
        .await?;
        return Ok(false);
    }

    tracing::debug!("Trying to show RISC-V debug module information");
    let info = match probe.try_get_riscv_interface_builder() {
        Ok(factory) => {
            let mut state = factory.create_state();
            factory
                .attach(&mut state)
                .map_err(RiscvError::from)
                .and_then(|mut interface| read_riscv_debug_module(&mut interface))
        }
        Err(error) => Err(error),
    };

    let event = match info {
        Ok(info) => InfoEvent::RiscvDebugModule(info),
        Err(error) if is_not_riscv_dtm(&error) => return Ok(false),
        Err(error) => InfoEvent::Error {
            architecture: "RISC-V".to_string(),
            error: format!("{error:?}"),
        },
    };
    ctx.publish::<TargetInfoDataTopic>(VarSeq::Seq2(0), &event)
        .await?;

    Ok(true)
}

fn is_not_riscv_dtm(error: &RiscvError) -> bool {
    matches!(
        error,
        RiscvError::NoRiscvTarget
            | RiscvError::UnsupportedDebugTransportModuleVersion(_)
            | RiscvError::UnsupportedDebugModuleVersion(DebugModuleVersion::NoModule)
    )
}

fn read_riscv_debug_module(
    interface: &mut RiscvCommunicationInterface,
) -> Result<RiscvDebugModuleInfo, RiscvError> {
    let info = interface.enter_debug_mode().map(|()| {
        let version = convert::to_wire_debug_module_version(interface.debug_module_version());
        let harts = (0..interface.num_harts())
            .map(|index| RiscvHartInfo {
                index,
                isa: read_hart_isa(interface, index),
            })
            .collect();

        RiscvDebugModuleInfo { version, harts }
    });

    // `enter_debug_mode` activates the debug module before it can fail.
    if !info.as_ref().is_err_and(is_not_riscv_dtm)
        && let Err(error) = deactivate_debug_module(interface)
    {
        tracing::warn!("Unable to deactivate the RISC-V debug module: {error:?}");
    }

    info
}

/// Gives the debug module back in its reset state.
///
/// [`RiscvCommunicationInterface::disable_debug_module`] is not used, because it halts the hart.
fn deactivate_debug_module(interface: &mut RiscvCommunicationInterface) -> Result<(), RiscvError> {
    // `enter_debug_mode` asks hart 0 of a version 1.0 debug module to stay out of low-power
    // states.
    if interface.debug_module_version() == DebugModuleVersion::Version1_0 {
        let mut control = Dmcontrol(0);
        control.set_dmactive(true);
        control.set_clrkeepalive(true);
        interface.write_dm_register(control)?;
    }

    interface.write_dm_register(Dmcontrol(0))
}

fn read_hart_isa(interface: &mut RiscvCommunicationInterface, hart: u32) -> RiscvHartIsa {
    if !interface.hart_enabled(hart) {
        return RiscvHartIsa::Unavailable;
    }

    let isa = interface
        .select_hart(hart)
        .and_then(|_| interface.read_hart_isa());

    match isa {
        Ok(Some(isa)) => RiscvHartIsa::Isa {
            xlen: isa.xlen,
            extensions: isa.extensions,
        },
        Ok(None) => RiscvHartIsa::NotImplemented,
        Err(RiscvError::HartUnavailable) => RiscvHartIsa::Unavailable,
        Err(error) => RiscvHartIsa::Error(format!("{error:?}")),
    }
}

async fn show_xtensa_tap_info(ctx: &mut RpcContext, probe: &mut Probe) -> anyhow::Result<()> {
    if let Err(error) = try_read_xtensa_info(ctx, probe, WireProtocol::Jtag).await {
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

        // WCH-Link probes don't expose a real JTAG IDCODE; report the chip
        // family and ID from the probe's AttachChip response instead.
        if let Some(wch_info) = probe
            .try_into::<WchLink>()
            .map(|wlink| (wlink.chip_family(), wlink.chip_id()))
        {
            let (family, chip_id) = wch_info;
            ctx.publish::<TargetInfoDataTopic>(
                VarSeq::Seq2(0),
                &InfoEvent::Message(format!(
                    "RISC-V Chip:\n  Family:  {:#04x} ({family:?})\n  Chip ID: {chip_id:#010x}",
                    family as u8
                )),
            )
            .await?;
            return Ok(());
        }

        let idcode = {
            let factory = probe.try_get_riscv_interface_builder()?;
            let mut state = factory.create_state();
            let mut interface = factory.attach(&mut state)?;
            interface.read_idcode()?
        };
        show_riscv_info(ctx, idcode).await?;
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
    dp_address: dp::DpAddress,
    announce_dp: bool,
) -> (Probe, anyhow::Result<dp::DebugPortVersion>) {
    tracing::debug!("Trying to show ARM chip information");

    let mut interface = match probe
        .try_into_arm_debug_interface(DefaultArmSequence::create())
        .map_err(|(iface, e)| (iface, anyhow!(e)))
    {
        Ok(interface) => interface,
        Err((probe, e)) => return (probe, Err(e)),
    };

    if let Err(err) = interface.select_debug_port(dp_address) {
        return (interface.close(), Err(anyhow!(err)));
    }

    let res = show_arm_info(ctx, &mut *interface, dp_address, announce_dp).await;
    (interface.close(), res)
}

/// Try to show information about the ARM chip, connected to a DP at the given address.
///
/// Shows the DP address in a heading when `announce_dp` is set.
///
/// Returns the version of the DP.
async fn show_arm_info(
    ctx: &mut RpcContext,
    interface: &mut dyn ArmDebugInterface,
    dp: dp::DpAddress,
    announce_dp: bool,
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
            dp_info: convert::to_wire_debug_port_id(&dp_info),
            targetid,
            dlpidr,
        }
    } else {
        DebugPortInfoNode {
            dp_info: convert::to_wire_debug_port_id(&dp_info),
            targetid: 0,
            dlpidr: 0,
        }
    };

    let mut info = DebugPortInfo {
        dp_info: dpinfo.clone(),
        aps: vec![],
    };

    if announce_dp {
        ctx.publish::<TargetInfoDataTopic>(
            VarSeq::Seq2(0),
            &InfoEvent::Message(format!("ARM Chip with debug port {dp:x?}:")),
        )
        .await?;
    }

    if dp_info.version != dp::DebugPortVersion::DPv3 {
        let access_ports = interface.access_ports(dp)?;
        for ap_address in access_ports {
            match ap_address.ap() {
                ApAddress::V1(_) => {
                    let raw_idr = interface.read_raw_ap_register(&ap_address, IDR::ADDRESS)?;
                    let idr: IDR = raw_idr.try_into()?;

                    let ap_info = if idr.CLASS() == ApClass::MemAp {
                        let mut ap_nodes = ComponentTreeNode::new(format!(
                            "{} MemoryAP ({:?})",
                            ap_address.ap_v1()?,
                            idr.TYPE()
                        ));
                        if let Err(e) = handle_memory_ap(interface, &ap_address, &mut ap_nodes) {
                            ap_nodes.push(format!("Error during access: {e}"));
                        };
                        ApInfo::MemoryAp {
                            ap_addr: FullyQualifiedApAddress {
                                dp: convert::to_wire_dp_address(ap_address.dp()),
                                ap: ap_address.ap().to_string(),
                            },
                            component_tree: ap_nodes,
                        }
                    } else {
                        ApInfo::Unknown {
                            ap_addr: FullyQualifiedApAddress {
                                dp: convert::to_wire_dp_address(ap_address.dp()),
                                ap: ap_address.ap().to_string(),
                            },
                            idr: raw_idr,
                        }
                    };

                    info.aps.push(ap_info);
                }

                ApAddress::V2(_) => {
                    unreachable!("Ap V1 and V2 cannot be mixed.")
                }
            }
        }
    } else {
        let fqa = arm::FullyQualifiedApAddress::v2_with_dp(dp, ApV2Address::root());
        let root_rom_table = {
            let mut root_memory = interface.memory_interface(&fqa)?;
            let base_address = root_memory.base_address()?;
            Component::try_parse(&mut *root_memory, base_address)?
        };
        let mut component_tree = ComponentTreeNode::new(String::new());
        coresight_component_tree(interface, root_rom_table, &fqa, &mut component_tree)?;
        info.aps.push(ApInfo::ApV2Root { component_tree });
    }

    ctx.publish::<TargetInfoDataTopic>(VarSeq::Seq2(0), &InfoEvent::ArmDp(info))
        .await?;

    Ok(dp_info.version)
}

fn handle_memory_ap(
    interface: &mut dyn ArmDebugInterface,
    access_port: &arm::FullyQualifiedApAddress,
    parent: &mut ComponentTreeNode,
) -> anyhow::Result<()> {
    let component = {
        let raw_idr = interface.read_raw_ap_register(access_port, IDR::ADDRESS)?;
        let idr: IDR = raw_idr.try_into()?;
        let mut memory = interface.memory_interface(access_port)?;

        // Check if the AP is accessible
        let csw = memory.generic_status()?;
        if !csw.DeviceEn() {
            *parent = ComponentTreeNode::new(
                "Memory AP is not accessible, DeviceEn bit not set".to_string(),
            );
            return Ok(());
        }

        if matches!(
            idr.TYPE(),
            ApType::AmbaAhb3 | ApType::AmbaAhb5 | ApType::AmbaAhb5Hprot
        ) {
            // Enable DWT here otherwise DWT/ITM/ETM/TPIU won't be visible in component table.
            // DWT is part of the Cortex-M core and unlikely to show up on a non-AHB bus.
            // Enabling `DWTENA` on e.g. an APB bus will cause the port to error out.
            let mut demcr = Demcr(memory.read_word_32(Demcr::get_mmio_address())?);
            demcr.set_dwtena(true);
            memory.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        }

        let base_address = memory.base_address()?;
        Component::try_parse(&mut *memory, base_address)?
    };
    coresight_component_tree(interface, component, access_port, parent)
}

fn coresight_component_tree(
    interface: &mut dyn ArmDebugInterface,
    component: Component,
    access_port: &arm::FullyQualifiedApAddress,
    parent: &mut ComponentTreeNode,
) -> anyhow::Result<()> {
    match &component {
        Component::GenericVerificationComponent(id) => {
            parent.push(ComponentTreeNode::new(format!(
                "{:#06x} Generic",
                id.component_address()
            )));
        }
        Component::Class1RomTable(id, table) => {
            let peripheral_id = id.peripheral_id();

            let root = if let Some(part) = peripheral_id.determine_part() {
                format!("{} (ROM Table, Class 1)", part.name())
            } else {
                let designer = peripheral_id.designer().unwrap_or("<unknown>");
                format!(
                    "ROM Table (Class 1), Designer: {}, Part: {:#06x}, Devtype: {:#04x}, Archid: {:#06x}",
                    designer,
                    peripheral_id.part(),
                    peripheral_id.dev_type(),
                    peripheral_id.arch_id(),
                )
            };

            let mut tree =
                ComponentTreeNode::new(format!("{:#06x} {}", id.component_address(), root));
            process_vendor_rom_tables(interface, id, table, access_port, &mut tree)?;

            for entry in table.entries() {
                let component = entry.component().clone();

                coresight_component_tree(interface, component, access_port, &mut tree)?;
            }
            parent.push(tree);
        }
        Component::CoresightComponent(id) => {
            let peripheral_id = id.peripheral_id();
            let part_info = peripheral_id.determine_part();

            let component_description = if let Some(part_info) = part_info {
                format!("{: <15} (Coresight Component)", part_info.name())
            } else {
                format!(
                    "Coresight Component, Part: {:#06x}, Devtype: {:#04x}, Archid: {:#06x}, Designer: {}",
                    peripheral_id.part(),
                    peripheral_id.dev_type(),
                    peripheral_id.arch_id(),
                    peripheral_id.designer().unwrap_or("<unknown>"),
                )
            };

            let mut tree = ComponentTreeNode::new(format!(
                "{:#06x} {}",
                id.component_address(),
                component_description
            ));
            let is_rom = part_info
                .map(|p| p.peripheral_type() == PeripheralType::Rom)
                .unwrap_or(false);
            process_component_entry(
                if is_rom { &mut *parent } else { &mut tree },
                interface,
                peripheral_id,
                &component,
                access_port,
            )?;
            parent.push(tree);
        }

        Component::PeripheralTestBlock(id) => {
            parent.push(ComponentTreeNode::new(format!(
                "{:#06x} Peripheral test block",
                id.component_address()
            )));
        }
        Component::GenericIPComponent(id) => {
            let peripheral_id = id.peripheral_id();

            let desc = if let Some(part_desc) = peripheral_id.determine_part() {
                format!(
                    "{:#06x} {: <15} (Generic IP Component)",
                    id.component_address(),
                    part_desc.name()
                )
            } else {
                "Generic IP Component".to_string()
            };

            let mut tree = ComponentTreeNode::new(desc);
            process_component_entry(&mut tree, interface, peripheral_id, &component, access_port)?;
            parent.push(tree);
        }

        Component::CoreLinkOrPrimeCellOrSystemComponent(id) => {
            let desc = "Core Link / Prime Cell / System Component";
            let peripheral_id = id.peripheral_id();
            let part_info = peripheral_id.determine_part();

            let component_description = if let Some(part_info) = part_info {
                format!("{: <15} ({})", part_info.name(), desc)
            } else {
                format!(
                    "{}, Part: {:#06x}, Devtype: {:#04x}, Archid: {:#06x}, Designer: {}",
                    desc,
                    peripheral_id.part(),
                    peripheral_id.dev_type(),
                    peripheral_id.arch_id(),
                    peripheral_id.designer().unwrap_or("<unknown>"),
                )
            };

            parent.push(ComponentTreeNode::new(format!(
                "{:#06x} {}",
                id.component_address(),
                component_description
            )));
        }
    };

    Ok(())
}

/// Processes information from/around manufacturer-specific ROM tables and adds them to the tree.
///
/// Some manufacturer-specific ROM tables contain more than just entries. This function tries
/// to make sense of these tables.
fn process_vendor_rom_tables(
    interface: &mut dyn ArmDebugInterface,
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
    interface: &mut dyn ArmDebugInterface,
    peripheral_id: &PeripheralID,
    component: &Component,
    access_port: &arm::FullyQualifiedApAddress,
) -> anyhow::Result<()> {
    let Some(part) = peripheral_id.determine_part() else {
        return Ok(());
    };

    match part.peripheral_type() {
        PeripheralType::Scs => {
            let cc = &CoresightComponent::new(component.clone(), access_port.clone());
            let scs = &mut Scs::new(interface, cc);
            let cpu_tree = cpu_info_tree(scs)?;

            tree.push(cpu_tree);
        }
        PeripheralType::MemAp => {
            let dp = access_port.dp();
            let ApAddress::V2(addr) = access_port.ap() else {
                unreachable!("This should only happen on ap v2 addresses.");
            };
            if addr.0.is_some() {
                return Err(anyhow::anyhow!("Nested memory APs are not yet supported."));
            }
            let addr = arm::FullyQualifiedApAddress::v2_with_dp(
                dp,
                arm::ApV2Address::new(component.id().component_address()),
            );
            handle_memory_ap(interface, &addr, tree)?;
        }
        PeripheralType::Rom => {
            let id = component.id();
            let mut memory = interface.memory_interface(access_port)?;
            let rom_table = RomTable::try_parse(
                memory.as_mut() as &mut dyn ArmMemoryInterface,
                id.component_address(),
            )?;
            drop(memory);

            process_vendor_rom_tables(interface, id, &rom_table, access_port, tree)?;
            for entry in rom_table.entries() {
                let component = entry.component().clone();

                coresight_component_tree(interface, component, access_port, tree)?;
            }
        }
        _ => {}
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

async fn show_riscv_info(ctx: &mut RpcContext, idcode: Option<u32>) -> anyhow::Result<()> {
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
            idcode,
        },
    )
    .await
}

pub(crate) mod convert {
    use super::{
        DebugPortId, DebugPortVersion, DpAddress, MinDpSupport, RiscvDebugModuleVersion,
        TargetInfoRequest,
    };
    use crate::rpc::functions::chip::convert::to_wire_jep106_code;
    use crate::rpc::functions::probe::convert::from_wire_debug_probe_selector;
    use crate::util::common_options::ProbeOptions;
    use probe_rs::{
        architecture::{arm::dp, riscv::communication_interface::DebugModuleVersion},
        probe::WireProtocol as ProbeRsWireProtocol,
    };
    use probe_rs_rpc::probe::WireProtocol;

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
                probe: Some(from_wire_debug_probe_selector(request.probe.selector())),
                speed: request.speed,
                connect_under_reset: request.connect_under_reset,
                cycle_power: false,
                dry_run: request.dry_run,
                allow_erase_all: false,
                attach_timeout: None,
            }
        }
    }

    pub(crate) fn to_wire_dp_address(address: dp::DpAddress) -> DpAddress {
        match address {
            dp::DpAddress::Default => DpAddress::Default,
            dp::DpAddress::Multidrop(target_sel) => DpAddress::Multidrop(target_sel),
        }
    }

    pub(crate) fn to_wire_debug_port_id(id: &dp::DebugPortId) -> DebugPortId {
        DebugPortId {
            revision: id.revision,
            part_no: id.part_no,
            version: to_wire_debug_port_version(id.version),
            min_dp_support: match id.min_dp_support {
                dp::MinDpSupport::NotImplemented => MinDpSupport::NotImplemented,
                dp::MinDpSupport::Implemented => MinDpSupport::Implemented,
            },
            designer: to_wire_jep106_code(id.designer),
        }
    }

    pub(crate) fn to_wire_debug_port_version(version: dp::DebugPortVersion) -> DebugPortVersion {
        match version {
            dp::DebugPortVersion::DPv0 => DebugPortVersion::DPv0,
            dp::DebugPortVersion::DPv1 => DebugPortVersion::DPv1,
            dp::DebugPortVersion::DPv2 => DebugPortVersion::DPv2,
            dp::DebugPortVersion::DPv3 => DebugPortVersion::DPv3,
            dp::DebugPortVersion::Unsupported(v) => DebugPortVersion::Unsupported(v),
        }
    }

    pub(crate) fn to_wire_debug_module_version(
        version: DebugModuleVersion,
    ) -> RiscvDebugModuleVersion {
        let (major, minor) = match version {
            DebugModuleVersion::NoModule => return RiscvDebugModuleVersion::NoModule,
            DebugModuleVersion::NonConforming => return RiscvDebugModuleVersion::NonConforming,
            DebugModuleVersion::Unknown(v) => return RiscvDebugModuleVersion::Unknown(v),
            DebugModuleVersion::Version0_11 => (0, 11),
            DebugModuleVersion::Version0_13 => (0, 13),
            DebugModuleVersion::Version1_0 => (1, 0),
        };
        RiscvDebugModuleVersion::Version { major, minor }
    }
}

#[cfg(test)]
mod tests {
    use super::{JEP_ST, JEP_TENSILICA, JtagTapInfo, TapKind};

    fn kind(ir_len: u8, idcode: Option<u32>) -> TapKind {
        TapKind::classify(&JtagTapInfo { idcode, ir_len })
    }

    #[test]
    fn jep_codes() {
        assert_eq!(JEP_ST.get(), Some("STMicroelectronics"));
        assert_eq!(JEP_TENSILICA.get(), Some("Tensilica"));
    }

    #[test]
    fn classify_taps() {
        assert_eq!(kind(4, Some(0x4ba0_0477)), TapKind::Arm);
        assert_eq!(kind(4, None), TapKind::Arm);
        assert_eq!(kind(3, Some(0x0000_0001)), TapKind::Unsupported);
        // ESP32
        assert_eq!(kind(5, Some(0x1200_34e5)), TapKind::Xtensa);
        // ESP32-C3
        assert_eq!(kind(5, Some(0x0000_5c25)), TapKind::Riscv);
        assert_eq!(kind(5, None), TapKind::RiscvOrXtensa);
        // STM32F4 boundary scan
        assert_eq!(kind(5, Some(0x0641_3041)), TapKind::Unsupported);
        // Xilinx Zynq-7000 PL
        assert_eq!(kind(6, Some(0x0372_7093)), TapKind::Unsupported);
        assert_eq!(kind(8, None), TapKind::Unsupported);
    }
}
