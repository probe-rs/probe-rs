//! # DUT Definitions
//!
//! This module handles the definition of the different devices under test (DUTs),
//! which are used by the tester.

use miette::IntoDiagnostic;
use miette::Result;
use miette::WrapErr;
use probe_rs::config::Registry;
use probe_rs::{
    Target,
    probe::{DebugProbeSelector, Probe, list::Lister},
};
use serde::Deserialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
struct RawDutDefinition {
    chip: String,
    /// Selector for the debug probe to be used.
    /// See [probe_rs::probe::DebugProbeSelector].
    probe_selector: String,
    probe_speed: Option<u32>,

    flash_test_binary: Option<String>,

    #[serde(default)]
    reset_connected: bool,
}

impl RawDutDefinition {
    /// Try to parse a DUT definition from a file.
    fn from_file(file: &Path) -> Result<Self> {
        let file_content = std::fs::read_to_string(file).into_diagnostic()?;

        let definition: RawDutDefinition = toml::from_str(&file_content).into_diagnostic()?;

        Ok(definition)
    }
}

#[derive(Debug, Clone)]
pub enum DefinitionSource {
    File(PathBuf),
    Cli,
}

#[derive(Debug, Clone)]
pub struct DutDefinition {
    pub chip: Target,

    /// Selector for the debug probe to be used.
    /// See [probe_rs::probe::DebugProbeSelector].
    ///
    /// If not set, any detected probe will be used.
    /// If multiple probes are found, an error is returned.
    pub probe_selector: Option<DebugProbeSelector>,

    /// Probe speed in kHz.
    ///
    /// If not set, the default speed of the probe will be used.
    pub probe_speed: Option<u32>,

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
    pub fn new(chip: &str, probe: &str) -> miette::Result<Self> {
        let target = lookup_unique_target(chip)?;

        let selector: DebugProbeSelector = probe.parse().into_diagnostic()?;

        Ok(DutDefinition {
            chip: target,
            probe_selector: Some(selector),
            probe_speed: None,
            flash_test_binary: None,
            source: DefinitionSource::Cli,
            reset_connected: false,
        })
    }

    pub fn autodetect_probe(chip: &str) -> miette::Result<Self> {
        let target = lookup_unique_target(chip)?;

        Ok(DutDefinition {
            chip: target,
            probe_selector: None,
            probe_speed: None,
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

        miette::ensure!(
            directory.is_dir(),
            "Unable to collect target definitions from path '{}'. Path is not a directory.",
            directory.display()
        );

        let mut definitions = Vec::new();

        for file in directory.read_dir().into_diagnostic()? {
            let file_path = file.into_diagnostic()?.path();

            // Ignore files without .toml ending
            if file_path.extension() != Some(OsStr::new("toml")) {
                log::debug!(
                    "Skipping file {}, does not end with .toml",
                    file_path.display(),
                );
                continue;
            }

            let definition = DutDefinition::from_file(&file_path).wrap_err_with(|| {
                format!("Failed to parse definition '{}'", file_path.display())
            })?;

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
        let lister = Lister::new();

        let mut probe = match &self.probe_selector {
            Some(selector) => lister
                .open(selector)
                .into_diagnostic()
                .wrap_err_with(|| format!("Failed to open probe with selector {selector}"))?,
            None => {
                let probes = lister.list_all();

                miette::ensure!(!probes.is_empty(), "No probes detected!");

                miette::ensure!(
                    probes.len() < 2,
                    "Multiple probes detected. Specify which probe to use using the '--probe' argument."
                );

                probes[0].open().into_diagnostic()?
            }
        };

        if let Some(probe_speed) = self.probe_speed {
            probe.set_speed(probe_speed).into_diagnostic()?;
        }

        Ok(probe)
    }

    fn from_raw_definition(raw_definition: RawDutDefinition, source_file: &Path) -> Result<Self> {
        let probe_selector = Some(raw_definition.probe_selector.try_into().into_diagnostic()?);

        let target = lookup_unique_target(&raw_definition.chip)?;

        let flash_test_binary = if let Some(path) = &raw_definition.flash_test_binary {
            let mut path = PathBuf::from(path);
            if !path.is_absolute() {
                path = source_file
                    .parent()
                    .expect("Source file should have a parent")
                    .join(path);
            }

            Some(path)
        } else {
            None
        };

        Ok(Self {
            chip: target,
            probe_speed: raw_definition.probe_speed,
            probe_selector,
            flash_test_binary,
            source: DefinitionSource::File(source_file.to_owned()),
            reset_connected: raw_definition.reset_connected,
        })
    }
}

fn lookup_unique_target(chip: &str) -> Result<Target> {
    let registry = Registry::from_builtin_families();
    let target = registry.get_target_by_name(chip).into_diagnostic()?;

    if !target.name.eq_ignore_ascii_case(chip) {
        miette::bail!(
            "Chip definition does not match exactly, the chip is specified as {}, but the entry in the registry is {}",
            chip,
            target.name
        );
    }

    Ok(target)
}

#[test]
fn find_unique_target() {
    let target = lookup_unique_target("nRF52840_xxAA").unwrap();

    assert_eq!(target.name, "nRF52840_xxAA");
}

#[test]
fn find_unique_target_failure() {
    // this should fail because the full name is nRF52840_xxAA
    lookup_unique_target("nRF52840_xx").unwrap_err();
}

#[test]
fn find_unique_target_with_non_unique_prefix() {
    // There is also a chip named "esp32c6_lp" in the registry, this ensures
    // that looking up just "esp32c6" still works.
    let target = lookup_unique_target("esp32c6").unwrap();

    assert_eq!(target.name, "esp32c6");
}
