use crate::serialize::hex_option;
use serde::{Deserialize, Serialize};

use crate::ApAddress;

/// A memory access port which allows access to system memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPort {
    /// The name of the memory access port.
    pub name: String,

    /// Options for a memory access port.
    pub memory_port_options: MemoryPortOptions,
}

/// Options for a memory access port.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum MemoryPortOptions {
    /// ARM specific memory port options.
    Arm(ArmMemoryPortOptions),
}

/// The data required to access an ARM core
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArmMemoryPortOptions {
    /// The access port number to access the memory.
    pub ap: ApAddress,

    /// The JTAG TAP index to access the memory.
    pub jtag_tap: Option<usize>,

    /// The TARGETSEL value used to access the core
    #[serde(serialize_with = "hex_option")]
    pub targetsel: Option<u32>,
}
