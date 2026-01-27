use std::collections::HashMap;

use super::memory::MemoryRegion;
use crate::{CoreType, serialize::hex_option};
use serde::{Deserialize, Serialize};

/// Represents a DAP scan chain element.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanChainElement {
    /// Unique name of the DAP
    pub name: Option<String>,
    /// Specifies the IR length of the DAP (default value: 4).
    pub ir_len: Option<u8>,
}

impl ScanChainElement {
    /// Returns the IR length, or 4 if not specified.
    pub fn ir_len(&self) -> u8 {
        self.ir_len.unwrap_or(4)
    }
}

/// Configuration for JTAG tunneling.
///
/// This JTAG tunnel wraps JTAG IR and DR accesses as DR access to a specific instruction. For
/// example, this can be used to access a Risc-V core in an FPGA using the same JTAG cable that
/// configures the FPGA.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RiscvJtagTunnel {
    /// JTAG instruction used to tunnel
    pub ir_id: u32,

    /// Width of tunneled JTAG instruction register
    pub ir_width: u32,
}

/// Configuration for JTAG probes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Jtag {
    /// Describes the scan chain
    ///
    /// ref: `<https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/sdf_pg.html#sdf_element_scanchain>`
    #[serde(default)]
    pub scan_chain: Option<Vec<ScanChainElement>>,

    /// Describes JTAG tunnel for Risc-V
    #[serde(default)]
    pub riscv_tunnel: Option<RiscvJtagTunnel>,
}

/// A single chip variant.
///
/// This describes an exact chip variant, including the cores, flash and memory size. For example,
/// the `nRF52832` chip has two variants, `nRF52832_xxAA` and `nRF52832_xxBB`. For this case,
/// the struct will correspond to one of the variants, e.g. `nRF52832_xxAA`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info` command.
    pub part: Option<u16>,
    /// An URL to the SVD file for this chip.
    pub svd: Option<String>,
    /// Documentation URLs associated with this chip.
    #[serde(default)]
    pub documentation: HashMap<String, String>,
    /// The package variants available for this chip.
    ///
    /// If empty, the chip is assumed to have only one package variant.
    #[serde(default)]
    pub package_variants: Vec<String>,
    /// The cores available on the chip.
    #[serde(default)]
    pub cores: Vec<Core>,
    /// The memory regions available on the chip.
    pub memory_map: Vec<MemoryRegion>,
    /// Names of all flash algorithms available for this chip.
    ///
    /// This can be used to look up the flash algorithm in the
    /// [`ChipFamily::flash_algorithms`] field.
    ///
    /// [`ChipFamily::flash_algorithms`]: crate::ChipFamily::flash_algorithms
    #[serde(default)]
    pub flash_algorithms: Vec<String>,
    /// Specific memory ranges to search for a dynamic RTT header for code
    /// running on this chip.
    ///
    /// This need not be specified for most chips because the default is
    /// to search all RAM regions specified in `memory_map`. However,
    /// that behavior isn't appropriate for some chips, such as those which
    /// have a very large amount of RAM that would be time-consuming to
    /// scan exhaustively.
    ///
    /// If specified then this is a list of zero or more address ranges to
    /// scan. Each address range must be enclosed in exactly one RAM region
    /// from `memory_map`. An empty list disables automatic scanning
    /// altogether, in which case RTT will be enabled only when using an
    /// executable image that includes the `_SEGGER_RTT` symbol pointing
    /// to the exact address of the RTT header.
    pub rtt_scan_ranges: Option<Vec<std::ops::Range<u64>>>,
    /// JTAG-specific options
    #[serde(default)]
    pub jtag: Option<Jtag>,
    /// The default binary format for this chip
    // TODO: rename to default_platform
    #[serde(default)]
    pub default_binary_format: Option<String>,
}

impl Chip {
    /// Create a generic chip with the given name, a single core,
    /// and no flash algorithm or memory map. Used to create
    /// generic targets.
    pub fn generic_arm(name: &str, core_type: CoreType) -> Self {
        Chip {
            name: name.to_string(),
            part: None,
            svd: None,
            documentation: HashMap::new(),
            package_variants: vec![],
            cores: vec![Core {
                name: "main".to_string(),
                core_type,
                core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions::default()),
            }],
            memory_map: vec![],
            flash_algorithms: vec![],
            rtt_scan_ranges: None,
            jtag: None,
            default_binary_format: None,
        }
    }

    /// Returns the package variants for this chip.
    pub fn package_variants(&self) -> impl Iterator<Item = &String> {
        std::slice::from_ref(&self.name)
            .iter()
            .chain(self.package_variants.iter())
    }
}

/// An individual core inside a chip
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Core {
    /// The core name.
    pub name: String,

    /// The core type.
    /// E.g. `M0` or `M4`.
    #[serde(rename = "type")]
    pub core_type: CoreType,

    /// The AP number to access the core
    pub core_access_options: CoreAccessOptions,
}

/// The data required to access a core
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreAccessOptions {
    /// ARM specific options
    Arm(ArmCoreAccessOptions),
    /// RISC-V specific options
    Riscv(RiscvCoreAccessOptions),
    /// Xtensa specific options
    Xtensa(XtensaCoreAccessOptions),
}

/// An address for AP accesses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ApAddress {
    /// References an address for an APv1 access, which is part of the ADIv5 specification.
    #[serde(rename = "v1")]
    V1(u8),
    /// References an address for an APv2 access, which is part of the ADIv6 specification.
    ///
    /// # Note
    /// This represents a base address within the root DP memory space.
    #[serde(rename = "v2")]
    V2(u64),
}

impl Default for ApAddress {
    fn default() -> Self {
        ApAddress::V1(0)
    }
}

/// The data required to access an ARM core
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArmCoreAccessOptions {
    /// The access port number to access the core
    pub ap: ApAddress,
    /// The TARGETSEL value used to access the core
    #[serde(serialize_with = "hex_option")]
    pub targetsel: Option<u32>,
    /// The base address of the debug registers for the core.
    /// Required for Cortex-A, optional for Cortex-M
    #[serde(serialize_with = "hex_option")]
    pub debug_base: Option<u64>,
    /// The base address of the cross trigger interface (CTI) for the core.
    /// Required in ARMv8-A
    #[serde(serialize_with = "hex_option")]
    pub cti_base: Option<u64>,

    /// The JTAG TAP index of the core's debug module
    pub jtag_tap: Option<usize>,
}

/// The data required to access a Risc-V core
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiscvCoreAccessOptions {
    /// The hart id
    pub hart_id: Option<u32>,

    /// The JTAG TAP index of the core's debug module
    pub jtag_tap: Option<usize>,
}

/// The data required to access an Xtensa core
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct XtensaCoreAccessOptions {
    /// The JTAG TAP index of the core's debug module
    pub jtag_tap: Option<usize>,
}
