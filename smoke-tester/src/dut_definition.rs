use anyhow::{bail, ensure, Context, Result};
use probe_rs::{
    config::{get_target_by_name, search_chips},
    DebugProbeSelector, Probe, Target,
};
use serde::Deserialize;
use std::{
    convert::TryInto,
    ffi::OsStr,
    path::{Path, PathBuf},
};
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

    flash_test_binary: Option<String>,

    #[serde(default)]
    reset_connected: bool,
}

impl RawDutDefinition {
    /// Try to parse a DUT definition from a file.
    fn from_file(file: &Path) -> Result<Self> {
        let file_content = std::fs::read(file)?;

        let definition: RawDutDefinition = toml::from_slice(&file_content)?;

        Ok(definition)
    }
}

#[derive(Clone)]
pub enum DefinitionSource {
    File(PathBuf),
    Cli,
}

#[derive(Clone)]
pub struct DutDefinition {
    pub chip: Target,

    /// Selector for the debug probe to be used.
    /// See [probe_rs::DebugProbeSelector].
    ///
    /// If not set, any detected probe will be used.
    /// If multiple probes are found, an error is returned.
    pub probe_selector: Option<DebugProbeSelector>,

    /// Path to a binary which can be used to test
    /// flashing for the DUT.     
    pub flash_test_binary: Option<PathBuf>,

    /// Source of the DUT definition.
    pub source: DefinitionSource,

    /// Indicates if the probe can control the reset pin of the
    /// DUT.
    pub reset_connected: bool,
}

impl DutDefinition {
    pub fn new(chip: &str, probe: &str) -> Result<Self> {
        let target = lookup_unique_target(chip)?;

        let selector: DebugProbeSelector = probe.parse()?;

        Ok(DutDefinition {
            chip: target,
            probe_selector: Some(selector),
            flash_test_binary: None,
            source: DefinitionSource::Cli,
            reset_connected: false,
        })
    }

    pub fn autodetect_probe(chip: &str) -> Result<Self> {
        let target = lookup_unique_target(chip)?;

        Ok(DutDefinition {
            chip: target,
            probe_selector: None,
            flash_test_binary: None,
            source: DefinitionSource::Cli,
            reset_connected: false,
        })
    }

    /// Collect all DUT definitions from a directory.
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

            // Ignore files without .toml ending
            if file_path.extension() != Some(OsStr::new("toml")) {
                log::debug!(
                    "Skipping file {}, does not end with .toml",
                    file_path.display(),
                );
                continue;
            }

            let definition = DutDefinition::from_file(&file_path)
                .with_context(|| format!("Failed to parse definition '{}'", file_path.display()))?;

            definitions.push(definition);
        }

        Ok(definitions)
    }

    /// Try to parse a DUT definition from a file.
    pub fn from_file(file: &Path) -> Result<Self> {
        let raw_definition = RawDutDefinition::from_file(file)?;

        DutDefinition::from_raw_definition(raw_definition, file)
    }

    pub fn open_probe(&self) -> Result<Probe> {
        match &self.probe_selector {
            Some(selector) => {
                let probe = Probe::open(selector.clone())
                    .with_context(|| format!("Failed to open probe with selector {}", selector))?;

                Ok(probe)
            }
            None => {
                let probes = Probe::list_all();

                ensure!(!probes.is_empty(), "No probes detected!");

                ensure!(
            probes.len() < 2,
            "Multiple probes detected. Specify which probe to use using the '--probe' argument."
        );

                let probe = probes[0].open()?;

                Ok(probe)
            }
        }
    }

    fn from_raw_definition(raw_definition: RawDutDefinition, source_file: &Path) -> Result<Self> {
        let probe_selector = Some(raw_definition.probe_selector.try_into()?);

        let target = lookup_unique_target(&raw_definition.chip)?;

        let flash_test_binary = raw_definition.flash_test_binary.map(PathBuf::from);

        let flash_test_binary = flash_test_binary.filter(|path| path.is_absolute());

        Ok(Self {
            chip: target,
            probe_selector,
            flash_test_binary,
            source: DefinitionSource::File(source_file.to_owned()),
            reset_connected: raw_definition.reset_connected,
        })
    }
}

fn lookup_unique_target(chip: &str) -> Result<Target> {
    let targets = search_chips(chip)?;

    ensure!(
        !targets.is_empty(),
        "Unable to find any chip matching {}",
        &chip
    );

    if targets.len() > 1 {
        eprintln!(
            "For tests, chip definition must be exact. Chip name {} matches multiple chips:",
            &chip
        );

        for target in &targets {
            eprintln!("\t{}", target);
        }

        bail!("Chip definition does not match exactly.");
    }

    let target = get_target_by_name(&targets[0])?;

    Ok(target)
}
