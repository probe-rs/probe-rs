pub mod cores;
pub mod targets;

use crate::target::Target;

pub fn get_target(name: impl Into<String>) -> Option<Target> {
    let map = hashmap!{
        "nrf51822" => self::targets::nrf51822::nRF51822,
    };

    let name = name.into();
    map.get(&name[..]).map(|creator| creator())

    // TODO: If not found try load chip from definition files (yaml, json, toml, you name it).
}