use crate::serialize::hex_option;
use serde::{Deserialize, Serialize};

use crate::ApAddress;

/// A port to the system memory bus that does not go through a core.
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

/// The data required to access an ARM memory port.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArmMemoryPortOptions {
    /// The access port number to access the memory.
    pub ap: ApAddress,

    /// The TARGETSEL value used to access the memory port.
    #[serde(serialize_with = "hex_option")]
    pub targetsel: Option<u32>,
}
