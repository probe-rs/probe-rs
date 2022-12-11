use super::*;
use probe_rs_target::ChipFamily;

pub(crate) struct Builtin(Vec<ChipFamily>);

impl Builtin {
    pub fn new() -> Self {
        const BUILTIN_TARGETS: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/targets.bincode"));

        let families: Vec<ChipFamily> = match bincode::deserialize(BUILTIN_TARGETS) {
            Ok(families) => families,
            Err(err) => panic!(
                "Failed to deserialize builtin targets. This is a bug : {:?}",
                err
            ),
        };

        for family in &families {
            family.validate().expect("builtin families must be valid");
        }

        Self(families)
    }
}

impl Provider for Builtin {
    fn name(&self) -> &str {
        "Built-in"
    }

    fn families(&self) -> Box<dyn Iterator<Item = Box<dyn Family + '_>> + '_> {
        Box::new(self.0.iter().map(|family| {
            let family: Box<dyn Family> = Box::new(family);
            family
        }))
    }
}
