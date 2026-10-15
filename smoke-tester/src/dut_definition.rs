//! # DUT Definitions
//!
//! This module handles the definition of the different devices under test (DUTs),
//! which are used by the tester.

use anyhow::{Context, Result};
use probe_rs::config::Registry;
use probe_rs::probe::WireProtocol;
use probe_rs::{
    Target,
    probe::{
        DebugProbeSelector, Probe,
        list::{Accessibility, Lister},
    },
};
use serde::Deserialize;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Time without power during a power cycle.
#[cfg(target_os = "linux")]
const POWER_OFF_TIME: Duration = Duration::from_secs(1);

/// Time to wait for the probe to come back after a power cycle.
const REENUMERATION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
struct RawDutDefinition {
    chip: String,
    /// Selector for the debug probe to be used.
    /// See [probe_rs::probe::DebugProbeSelector].
    probe_selector: String,
    probe_speed: Option<u32>,
    protocol: Option<WireProtocol>,

    flash_test_binary: Option<String>,

    #[serde(default)]
    reset_connected: bool,

    #[serde(default)]
    cycle_power: bool,
}

impl RawDutDefinition {
    /// Try to parse a DUT definition from a file.
    fn from_file(file: &Path) -> Result<Self> {
        let file_content = std::fs::read_to_string(file)?;

        let definition: RawDutDefinition = toml::from_str(&file_content)?;

        Ok(definition)
    }
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

    /// Protocol
    ///
    /// If not set, the default wire protocol will be used.
    pub protocol: Option<WireProtocol>,

    /// Path to a binary which can be used to test
    /// flashing for the DUT.
    pub flash_test_binary: Option<PathBuf>,

    /// Indicates if the probe can control the reset pin of the
    /// DUT.
    pub reset_connected: bool,

    /// Cycle the USB port power of the probe before each test.
    ///
    /// Only supported on Linux. The user needs write access to the
    /// `disable` file of the USB port in sysfs.
    pub cycle_power: bool,
}

impl DutDefinition {
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

        anyhow::ensure!(
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

    /// Check that the configured probe shows up when listing probes, and that the
    /// current user is allowed to access it.
    pub fn assert_listed(&self) -> Result<()> {
        let lister = Lister::new();
        let probes = lister.list_all_with_access();

        let listed = match &self.probe_selector {
            Some(selector) => probes
                .iter()
                .find(|probe| selector.matches_probe(&probe.info))
                .with_context(|| {
                    format!("Probe with selector {selector} did not show up when listing probes")
                })?,
            None => {
                anyhow::ensure!(
                    !probes.is_empty(),
                    "No probes detected when listing probes!"
                );
                &probes[0]
            }
        };

        anyhow::ensure!(
            listed.accessibility == Accessibility::Accessible,
            "Probe was listed but reported as not accessible: {}",
            listed.info
        );

        Ok(())
    }

    pub fn open_probe(&self) -> Result<Probe> {
        let lister = Lister::new();

        let mut probe = match &self.probe_selector {
            Some(selector) if self.cycle_power => {
                power_cycle(selector)?;
                open_after_power_cycle(&lister, selector)?
            }
            Some(selector) => lister
                .open(selector.clone())
                .with_context(|| format!("Failed to open probe with selector {selector}"))?,
            None => {
                let probes = lister.list_all();

                anyhow::ensure!(!probes.is_empty(), "No probes detected!");

                anyhow::ensure!(
                    probes.len() < 2,
                    "Multiple probes detected. Specify which probe to use using the '--probe' argument."
                );

                probes[0].open()?
            }
        };

        if let Some(probe_speed) = self.probe_speed {
            probe.set_speed(probe_speed)?;
        }

        if let Some(protocol) = self.protocol {
            probe.select_protocol(protocol)?;
        }

        Ok(probe)
    }

    fn from_raw_definition(raw_definition: RawDutDefinition, source_file: &Path) -> Result<Self> {
        let probe_selector = Some(raw_definition.probe_selector.parse()?);

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
            protocol: raw_definition.protocol,
            probe_selector,
            flash_test_binary,
            reset_connected: raw_definition.reset_connected,
            cycle_power: raw_definition.cycle_power,
        })
    }
}

/// Turn off the power of the USB port of the probe, then turn it on again.
#[cfg(target_os = "linux")]
fn power_cycle(selector: &DebugProbeSelector) -> Result<()> {
    use nusb::MaybeFuture;

    let device = nusb::list_devices()
        .wait()?
        .find(|device| selector.matches(device))
        .with_context(|| format!("Probe with selector {selector} not found"))?;

    // The port directory belongs to the hub. It stays when the probe disconnects.
    let disable = std::fs::canonicalize(device.sysfs_path().join("port"))
        .context("Failed to find the USB port of the probe")?
        .join("disable");

    std::fs::write(&disable, "1")
        .with_context(|| format!("Failed to turn off power at {}", disable.display()))?;
    std::thread::sleep(POWER_OFF_TIME);
    std::fs::write(&disable, "0")
        .with_context(|| format!("Failed to turn on power at {}", disable.display()))?;

    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn power_cycle(_selector: &DebugProbeSelector) -> Result<()> {
    anyhow::bail!("USB power cycling is only supported on Linux")
}

/// Open the probe. Retry until the probe enumerates again and udev
/// gives access to it.
fn open_after_power_cycle(lister: &Lister, selector: &DebugProbeSelector) -> Result<Probe> {
    let start = Instant::now();
    loop {
        match lister.open(selector.clone()) {
            Ok(probe) => return Ok(probe),
            Err(error) if start.elapsed() >= REENUMERATION_TIMEOUT => {
                return Err(error).with_context(|| {
                    format!("Failed to open probe with selector {selector} after power cycle")
                });
            }
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn lookup_unique_target(chip: &str) -> Result<Target> {
    let registry = Registry::from_builtin_families();
    let target = registry.get_target_by_name(chip)?;

    if !target.name.eq_ignore_ascii_case(chip) {
        anyhow::bail!(
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
