use super::*;
use probe_rs_target::{Chip, ChipFamily, CoreAccessOptions, CoreType, RiscvCoreAccessOptions};

pub(crate) struct Generic(Vec<ChipFamily>);

impl Generic {
    pub fn new() -> Self {
        Self(vec![
            ChipFamily {
                name: "Generic ARMv6-M".to_owned(),
                manufacturer: None,
                generated_from_pack: false,
                pack_file_release: None,
                variants: vec![
                    Chip::generic_arm("Cortex-M0", CoreType::Armv6m),
                    Chip::generic_arm("Cortex-M0+", CoreType::Armv6m),
                    Chip::generic_arm("Cortex-M1", CoreType::Armv6m),
                ],

                flash_algorithms: vec![],
                source: TargetDescriptionSource::Generic,
            },
            ChipFamily {
                name: "Generic ARMv7-M".to_owned(),
                manufacturer: None,
                generated_from_pack: false,
                pack_file_release: None,
                variants: vec![Chip::generic_arm("Cortex-M3", CoreType::Armv7m)],
                flash_algorithms: vec![],
                source: TargetDescriptionSource::Generic,
            },
            ChipFamily {
                name: "Generic ARMv7E-M".to_owned(),
                manufacturer: None,
                generated_from_pack: false,
                pack_file_release: None,
                variants: vec![
                    Chip::generic_arm("Cortex-M4", CoreType::Armv7em),
                    Chip::generic_arm("Cortex-M7", CoreType::Armv7em),
                ],
                flash_algorithms: vec![],
                source: TargetDescriptionSource::Generic,
            },
            ChipFamily {
                name: "Generic ARMv8-M".to_owned(),
                manufacturer: None,
                generated_from_pack: false,
                pack_file_release: None,
                variants: vec![
                    Chip::generic_arm("Cortex-M23", CoreType::Armv8m),
                    Chip::generic_arm("Cortex-M33", CoreType::Armv8m),
                    Chip::generic_arm("Cortex-M35P", CoreType::Armv8m),
                    Chip::generic_arm("Cortex-M55", CoreType::Armv8m),
                ],
                flash_algorithms: vec![],
                source: TargetDescriptionSource::Generic,
            },
            ChipFamily {
                name: "Generic RISC-V".to_owned(),
                manufacturer: None,
                pack_file_release: None,
                generated_from_pack: false,
                variants: vec![Chip {
                    name: "riscv".to_owned(),
                    part: None,
                    cores: vec![Core {
                        name: "core".to_owned(),
                        core_type: CoreType::Riscv,
                        core_access_options: CoreAccessOptions::Riscv(RiscvCoreAccessOptions {}),
                    }],
                    memory_map: vec![],
                    flash_algorithms: vec![],
                }],
                flash_algorithms: vec![],
                source: TargetDescriptionSource::Generic,
            },
        ])
    }
}

impl Provider for Generic {
    fn name(&self) -> &str {
        "Generic"
    }

    fn families(&self) -> Box<dyn Iterator<Item = Box<dyn Family + '_>> + '_> {
        Box::new(self.0.iter().map(|family| {
            let family: Box<dyn Family + '_> = Box::new(family);
            family
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate() {
        let generic = Generic::new();
        for family in generic.0 {
            family.validate().expect("must be valid");
        }
    }
}
