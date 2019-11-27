use crate::target::Core;
use std::collections::HashMap;

pub mod m0;
pub mod m33;
pub mod m4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexDump {
    pub regs: [u32; 16],
    stack_addr: u32,
    stack: Vec<u8>,
}

impl CortexDump {
    pub fn new(stack_addr: u32, stack: Vec<u8>) -> CortexDump {
        CortexDump {
            regs: [0u32; 16],
            stack_addr,
            stack,
        }
    }
}

pub fn get_core(name: impl AsRef<str>) -> Option<Box<dyn Core>> {
    let map: HashMap<&'static str, Box<dyn Core>> = hashmap! {
        "m0" => Box::new(self::m0::M0) as _,
        "m4" => Box::new(self::m4::M4) as _,
    };

    map.get(&name.as_ref().to_ascii_lowercase()[..]).cloned()
}
