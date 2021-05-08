use anyhow::{bail, ensure, Context, Result};
use probe_rs::{
    config::{get_target_by_name, search_chips},
    DebugProbeSelector, Probe, Target,
};
use serde::Deserialize;
use std::{convert::TryInto, path::Path};
///! # DUT Defintions
///!
///! This module handles the definition of the different devices under test (DUTs),
///! which are used by the tester.
///!

#[derive(Deserialize)]
struct RawDutDefinition {
    chip: String,
    /// Selector for the debug probe to be used.
    /// See [probe_rs::DebugProbeSelector].
    probe_selector: String,
}

impl RawDutDefinition {
    /// Try to parse a DUT definition from a file.
    fn from_file(file: &Path) -> Result<Self> {
        let file_content = std::fs::read(file)?;

        let definition: RawDutDefinition = toml::from_slice(&file_content)?;

        Ok(definition)
    }
}

pub struct DutDefinition {
    pub chip: Target,

    /// Selector for the debug probe to be used.
    /// See [probe_rs::DebugProbeSelector].
    pub probe_selector: DebugProbeSelector,
}

impl DutDefinition {
    /// Collect all DUT definitions from a direcotry.
    ///
    /// This will try to parse all TOML files in the given directory
    /// into DUT definitions.
    ///
    /// For TOML files which do not contain a valid DUT definition,
    /// an error is returned. Errors are also returned in case of
    /// IO errors, or if the given path is not a directory.
    pub fn collect(directory: impl AsRef<Path>) -> Result<Vec<DutDefinition>> {
        let directory = directory.as_ref();

        ensure!(
            directory.is_dir(),
            "Unable to collect target definitions from path '{}'. Path is not a directory.",
            directory.display()
        );

        let mut definitions = Vec::new();

        for file in directory.read_dir()? {
            let file_path = file?.path();

            let definition = DutDefinition::from_file(&file_path)
                .with_context(|| format!("Failed to parse definition '{}'", file_path.display()))?;

            definitions.push(definition);
        }

        Ok(definitions)
    }

    /// Try to parse a DUT definition from a file.
    fn from_file(file: &Path) -> Result<Self> {
        let raw_definition = RawDutDefinition::from_file(file)?;

        DutDefinition::from_raw_definition(raw_definition)
    }

    pub fn open_probe(&self) -> Result<Probe> {
        let probe = Probe::open(self.probe_selector.clone()).with_context(|| {
            format!(
                "Failed to open probe with selector {}",
                &self.probe_selector
            )
        })?;

        Ok(probe)
    }

    fn from_raw_definition(raw_definition: RawDutDefinition) -> Result<Self> {
        let probe_selector = raw_definition.probe_selector.try_into()?;

        let targets = search_chips(&raw_definition.chip)?;

        ensure!(
            !targets.is_empty(),
            "Unable to find any chip matching {}",
            &raw_definition.chip
        );

        if targets.len() > 1 {
            eprintln!(
                "For tests, chip definition must be exact. Chip name {} matches multiple chips:",
                &raw_definition.chip
            );

            for target in &targets {
                eprintln!("\t{}", target);
            }

            bail!("Chip definition does not match exactly.");
        }

        let target = get_target_by_name(&targets[0])?;

        Ok(Self {
            chip: target,
            probe_selector,
        })
    }
}
