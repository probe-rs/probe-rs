use postcard_schema::{Schema, schema};
use serde::{Deserialize, Serialize};

use crate::chip::{JEP106Code, MemoryRegion};
use crate::core_ops::WireCoreType;
use crate::probe::{DebugProbeEntry, WireProtocol};
use crate::{Key, RpcResult, Session};

/// Absolute flash sector range for GDB memory-map XML (`blocksize`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WireFlashSector {
    pub start: u64,
    pub length: u64,
    pub blocksize: u64,
}

/// Session-scoped target description fields the DAP RPC client needs without
/// mirroring the full server `probe_rs::Target` in its local registry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WireSessionTargetMetadata {
    pub target_name: String,
    pub default_format: Option<String>,
    pub cores: Vec<WireSessionCore>,
    pub memory_map: Vec<MemoryRegion>,
    pub flash_sectors: Vec<WireFlashSector>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub struct WireSessionCore {
    pub index: u32,
    pub core_type: WireCoreType,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct TargetMetadataRequest {
    pub sessid: Key<Session>,
}

#[derive(Serialize, Deserialize, Schema)]
pub struct TargetInfoRequest {
    pub probe: DebugProbeEntry,
    pub speed: Option<u32>,
    pub connect_under_reset: bool,
    pub dry_run: bool,
    pub target_sel: Option<u32>,
    pub protocol: WireProtocol,
    /// IR lengths for each TAP in the scan chain, in scan-chain order.
    ///
    /// When non-empty, the JTAG auto-detection scan is bypassed and these values are used
    /// directly. For example, `[5]` specifies a single-TAP chain with IR length 5.
    #[serde(default)]
    pub scan_chain: Vec<u8>,
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

/// Specifies if pushed-find operations are implemented or not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Schema)]
pub enum MinDpSupport {
    /// Pushed-find operations are **not** implemented.
    NotImplemented,
    /// Pushed-find operations are implemented.
    Implemented,
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
    ApV2Root {
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
    pub fn new(node: String) -> Self {
        Self {
            node,
            children: vec![],
        }
    }

    pub fn push(&mut self, child: impl Into<ComponentTreeNode>) {
        self.children.push(child.into());
    }
}

pub type TargetMetadataResponse = RpcResult<WireSessionTargetMetadata>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_session_target_metadata_roundtrip() {
        let metadata = WireSessionTargetMetadata {
            target_name: "nrf52840_xxAA".to_string(),
            default_format: Some("elf".to_string()),
            cores: vec![WireSessionCore {
                index: 0,
                core_type: WireCoreType::Armv7em,
            }],
            memory_map: vec![],
            flash_sectors: vec![WireFlashSector {
                start: 0,
                length: 0x1000,
                blocksize: 0x1000,
            }],
        };

        let encoded = postcard::to_allocvec(&metadata).unwrap();
        let decoded: WireSessionTargetMetadata = postcard::from_bytes(&encoded).unwrap();
        assert_eq!(decoded, metadata);
    }
}
