use super::memory::MemoryRegion;
use crate::CoreType;
use serde::{Deserialize, Serialize};

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
    #[cfg_attr(
        not(feature = "bincode"),
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub part: Option<u16>,
    /// The chip variant id.
    /// This id is used to detect the exact chip model in case the `PART` register entry is not unique.
    /// The origin of this value can differ between chip families and manufacturers due to different
    /// device identification approaches.
    #[cfg_attr(
        not(feature = "bincode"),
        serde(skip_serializing_if = "Option::is_none")
    )]
    pub id: Option<u32>,
    /// The cores available on the chip.
    pub cores: Vec<Core>,
    /// The memory regions available on the chip.
    pub memory_map: Vec<MemoryRegion>,
    /// Names of all flash algorithms available for this chip.
    ///
    /// This can be used to look up the flash algorithm in the
    /// [`ChipFamily::flash_algorithms`] field.
    ///
    /// [`ChipFamily::flash_algorithms`]: crate::ChipFamily::flash_algorithms
    pub flash_algorithms: Vec<String>,
}

impl Chip {
    /// Create a generic chip with the given name, a single core,
    /// and no flash algorithm or memory map. Used to create
    /// generic targets.
    pub fn generic_arm(name: &str, core_type: CoreType) -> Self {
        Chip {
            name: name.to_string(),
            part: None,
            id: None,
            cores: vec![Core {
                name: "main".to_string(),
                core_type,
                core_access_options: CoreAccessOptions::Arm(ArmCoreAccessOptions::default()),
            }],
            memory_map: vec![],
            flash_algorithms: vec![],
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
}

/// The data required to access a Risc-V core
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiscvCoreAccessOptions {}
