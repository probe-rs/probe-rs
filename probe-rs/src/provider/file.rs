use crate::config::RegistryError;
use crate::provider::{Family, Provider};
use probe_rs_target::{ChipFamily, TargetDescriptionSource};
use std::path::Path;

/// A `Provider` based on a YAML file.
pub struct File {
    name: String,
    chip_family: ChipFamily,
}

impl File {
    pub fn new(path_to_yaml: &Path) -> Result<Self, RegistryError> {
        let name = path_to_yaml.display().to_string();
        let file = std::fs::File::open(path_to_yaml)?;
        let mut chip_family: ChipFamily = serde_yaml::from_reader(file)?;

        // Ensure the source is external
        chip_family.source = TargetDescriptionSource::External;

        chip_family
            .validate()
            .map_err(|e| RegistryError::InvalidChipFamilyDefinition(chip_family.clone(), e))?;

        Ok(Self { name, chip_family })
    }
}

impl Provider for File {
    fn name(&self) -> &str {
        &self.name
    }

    fn families(&self) -> Box<dyn Iterator<Item = Box<dyn Family<'_> + '_>> + '_> {
        let family: Box<dyn Family> = Box::new(&self.chip_family);
        Box::new(std::iter::once(family))
    }
}
