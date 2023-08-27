use super::memory::MemoryRegion;
use crate::{serialize::hex_option, CoreType};
use serde::{Deserialize, Serialize};

/// Represents a DAP scan chain element.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanChainDap {
    /// Unique name of the DAP
    pub name: Option<String>,
    /// Specifies the type of the DAP (e.g. ARMCS-DP)
    pub dap_type: Option<String>,
    /// Specifies the IR length of the DAP (default value: 4).
    pub ir_len: Option<u8>,
    /// List of protocols via which the DAP can be use. Possible values are SWD, JTAG, and cJTAG.
    pub protocols: Option<Vec<String>>,
    /// Specifies the SW-DPv2 TARGETSEL register value that selects this DAP in a serial-wire debug multi-drop system
    pub targetsel: Option<u8>,
}

/// Represents a non DAP device in the scan chain. This can be other TAPs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScanChainDevice {
    /// Unique name of the device
    pub name: Option<String>,
    /// Specifies the type of the device (e.g. ARMCS-DP)
    pub device_type: Option<String>,
    /// Specifies the IR length of the device (default value: 4).
    pub ir_len: Option<u8>,
}

/// Declares the type of a scan chain as defined by CMSIS-Pack SDF
/// ref: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/sdf_pg.html#sdf_element_scanchain
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScanChain {
    /// Scan chain element is a DAP
    Dap(ScanChainDap),
    /// Scan chain element is another JTAG device such as other TAPs
    Device(ScanChainDevice),
}
/// A single chip variant.
///
/// This describes an exact chip variant, including the cores, flash and memory size. For example,
/// the `nRF52832` chip has two variants, `nRF52832_xxAA` and `nRF52832_xxBB`. For this case,
/// the struct will correspond to one of the variants, e.g. `nRF52832_xxAA`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chip {
    /// This is the name of the chip in base form.
    /// E.g. `nRF52832`.
    pub name: String,
    /// The `PART` register of the chip.
    /// This value can be determined via the `cli info` command.
    pub part: Option<u16>,
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
    /// Describes the scan chain
    ///
    /// ref: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/sdf_pg.html#sdf_element_scanchain
    #[serde(default)]
    pub scan_chain: Option<Vec<ScanChain>>,
}

impl Chip {
    /// Create a generic chip with the given name, a single core,
    /// and no flash algorithm or memory map. Used to create
    /// generic targets.
    pub fn generic_arm(name: &str, core_type: CoreType) -> Self {
        Chip {
            name: name.to_string(),
            part: None,
            cores: vec![Core {
                name: "main".to_string(),
                core_type,
                core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions::default()),
            }],
            memory_map: vec![],
            flash_algorithms: vec![],
            rtt_scan_ranges: None,
            scan_chain: Some(vec![]),
        }
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
    /// Arm specific options
    Arm(ArmCoreAccessOptions),
    /// Riscv specific options
    Riscv(RiscvCoreAccessOptions),
}

/// The data required to access an ARM core
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ArmCoreAccessOptions {
    /// The access port number to access the core
    pub ap: u8,
    /// The port select number to access the core
    pub psel: u32,
    /// The base address of the debug registers for the core.
    /// Required for Cortex-A, optional for Cortex-M
    #[serde(serialize_with = "hex_option")]
    pub debug_base: Option<u64>,
    /// The base address of the cross trigger interface (CTI) for the core.
    /// Required in ARMv8-A
    #[serde(serialize_with = "hex_option")]
    pub cti_base: Option<u64>,
}

/// The data required to access a Risc-V core
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiscvCoreAccessOptions {}
