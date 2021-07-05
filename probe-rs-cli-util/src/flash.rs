#![allow(unused_imports)]
use crate::{read_metadata, ArtifactError};

use std::{
    env,
    fs::File,
    io::{Error, Write},
    path::{Path, PathBuf},
    process,
    sync::Arc,
    time::Instant,
};

use probe_rs::{
    config::{RegistryError, TargetSelector},
    flashing::{
        DownloadOptions, FileDownloadError, FlashError, FlashLoader, FlashProgress, ProgressEvent,
    },
    DebugProbeError, DebugProbeSelector, FakeProbe, Probe, Session, Target, WireProtocol,
};
use structopt::StructOpt;

#[derive(Debug, StructOpt)]
pub struct FlashOptions {
    #[structopt(short = "V", long = "version")]
    pub version: bool,
    #[structopt(name = "chip", long = "chip")]
    pub chip: Option<String>,
    #[structopt(name = "chip description file path", long = "chip-description-path")]
    pub chip_description_path: Option<String>,
    #[structopt(name = "list-chips", long = "list-chips")]
    pub list_chips: bool,
    #[structopt(
        name = "list-probes",
        long = "list-probes",
        help = "Lists all the connected probes that can be seen.\n\
        If udev rules or permissions are wrong, some probes might not be listed."
    )]
    pub list_probes: bool,
    #[structopt(name = "disable-progressbars", long = "disable-progressbars")]
    pub disable_progressbars: bool,
    #[structopt(name = "protocol", long = "protocol", default_value = "swd")]
    pub protocol: WireProtocol,
    #[structopt(
        long = "probe",
        help = "Use this flag to select a specific probe in the list.\n\
        Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one probe with the same VID:PID."
    )]
    pub probe_selector: Option<DebugProbeSelector>,
    #[structopt(
        long = "connect-under-reset",
        help = "Use this flag to assert the nreset & ntrst pins during attaching the probe to the chip."
    )]
    pub connect_under_reset: bool,
    #[structopt(
        name = "reset-halt",
        long = "reset-halt",
        help = "Use this flag to reset and halt (instead of just a reset) the attached core after flashing the target."
    )]
    pub reset_halt: bool,
    #[structopt(
        name = "level",
        long = "log",
        help = "Use this flag to set the log level.\n\
        Default is `warning`. Possible choices are [error, warning, info, debug, trace]."
    )]
    pub log: Option<log::Level>,
    #[structopt(name = "speed", long = "speed", help = "The protocol speed in kHz.")]
    pub speed: Option<u32>,
    #[structopt(
        name = "restore-unwritten",
        long = "restore-unwritten",
        help = "Enable this flag to restore all bytes erased in the sector erase but not overwritten by any page."
    )]
    pub restore_unwritten: bool,
    #[structopt(
        name = "filename",
        long = "flash-layout",
        help = "Requests the flash builder to output the layout into the given file in SVG format."
    )]
    pub flash_layout_output_path: Option<String>,
    #[structopt(
        name = "elf file",
        long = "elf",
        help = "The path to the ELF file to be flashed."
    )]
    pub elf: Option<String>,
    #[structopt(
        name = "directory",
        long = "work-dir",
        help = "The work directory from which cargo-flash should operate from."
    )]
    pub work_dir: Option<String>,

    #[structopt(long = "dry-run")]
    pub dry_run: bool,

    #[structopt(flatten)]
    /// Arguments which are forwarded to 'cargo build'.
    pub cargo_options: CargoOptions,
}

#[derive(StructOpt, Debug)]
pub struct CargoOptions {
    #[structopt(name = "binary", long = "bin", hidden = true)]
    pub bin: Option<String>,
    #[structopt(name = "example", long = "example", hidden = true)]
    pub example: Option<String>,
    #[structopt(name = "package", short = "p", long = "package", hidden = true)]
    pub package: Option<String>,
    #[structopt(name = "release", long = "release", hidden = true)]
    pub release: bool,
    #[structopt(name = "target", long = "target", hidden = true)]
    pub target: Option<String>,
    #[structopt(
        name = "PATH",
        long = "manifest-path",
        parse(from_os_str),
        hidden = true
    )]
    pub manifest_path: Option<PathBuf>,
    #[structopt(long, hidden = true)]
    pub no_default_features: bool,
    #[structopt(long, hidden = true)]
    pub all_features: bool,
    #[structopt(long, hidden = true)]
    pub features: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CargoFlashError {
    #[error("No connected probes were found.")]
    NoProbesFound,
    #[error("Failed to list the target descriptions.")]
    FailedToReadFamilies(#[source] RegistryError),
    #[error("Failed to open the ELF file '{path}' for flashing.")]
    FailedToOpenElf {
        #[source]
        source: std::io::Error,
        path: String,
    },
    #[error("Failed to load the ELF data.")]
    FailedToLoadElfData(#[source] FileDownloadError),
    #[error("Failed to open the debug probe.")]
    FailedToOpenProbe(#[source] DebugProbeError),
    #[error("{number} probes were found.")]
    MultipleProbesFound { number: usize },
    #[error("The flashing procedure failed for '{path}'.")]
    FlashingFailed {
        #[source]
        source: FlashError,
        target: Target,
        target_spec: Option<String>,
        path: String,
    },
    #[error("Failed to parse the chip description '{path}'.")]
    FailedChipDescriptionParsing {
        #[source]
        source: RegistryError,
        path: String,
    },
    #[error("Failed to change the working directory to '{path}'.")]
    FailedToChangeWorkingDirectory {
        #[source]
        source: std::io::Error,
        path: String,
    },
    #[error("Failed to build the cargo project at '{path}'.")]
    FailedToBuildExternalCargoProject {
        #[source]
        source: ArtifactError,
        path: String,
    },
    #[error("Failed to build the cargo project.")]
    FailedToBuildCargoProject(#[source] ArtifactError),
    #[error("The chip '{name}' was not found in the database.")]
    ChipNotFound {
        #[source]
        source: RegistryError,
        name: String,
    },
    #[error("The protocol '{protocol}' could not be selected.")]
    FailedToSelectProtocol {
        #[source]
        source: DebugProbeError,
        protocol: WireProtocol,
    },
    #[error("The protocol speed coudl not be set to '{speed}' kHz.")]
    FailedToSelectProtocolSpeed {
        #[source]
        source: DebugProbeError,
        speed: u32,
    },
    #[error("Connecting to the chip was unsuccessful.")]
    AttachingFailed {
        #[source]
        source: probe_rs::Error,
        connect_under_reset: bool,
    },
    #[error("Failed to get a handle to the first core.")]
    AttachingToCoreFailed(#[source] probe_rs::Error),
    #[error("The reset of the target failed.")]
    TargetResetFailed(#[source] probe_rs::Error),
    #[error("The target could not be reset and halted.")]
    TargetResetHaltFailed(#[source] probe_rs::Error),
    #[error("Failed to write to file")]
    IOError(#[source] std::io::Error),
}

impl From<std::io::Error> for CargoFlashError {
    fn from(e: std::io::Error) -> Self {
        CargoFlashError::IOError(e)
    }
}

/// Lists all connected debug probes.
pub fn list_connected_probes(mut f: impl Write) -> Result<(), std::io::Error> {
    let probes = Probe::list_all();

    if !probes.is_empty() {
        writeln!(f, "The following debug probes were found:")?;
        for (num, link) in probes.iter().enumerate() {
            writeln!(f, "[{}]: {:?}", num, link)?;
        }
    } else {
        writeln!(f, "No debug probes were found.")?;
    }

    Ok(())
}

/// Print all the available families and their contained chips to the
/// commandline.
pub fn print_families(mut f: impl Write) -> Result<(), CargoFlashError> {
    writeln!(f, "Available chips:")?;
    for family in probe_rs::config::families().map_err(CargoFlashError::FailedToReadFamilies)? {
        writeln!(f, "{}", &family.name)?;
        writeln!(f, "    Variants:")?;
        for variant in family.variants() {
            writeln!(f, "        {}", variant.name)?;
        }
    }
    Ok(())
}

impl FlashOptions {
    /// Add targets contained in file given by --chip-description-path
    /// to probe-rs registery.
    pub fn try_load_chip_desc(&self) -> Result<(), CargoFlashError> {
        if let Some(ref cdp) = self.chip_description_path {
            probe_rs::config::add_target_from_yaml(&Path::new(cdp)).map_err(|error| {
                CargoFlashError::FailedChipDescriptionParsing {
                    source: error,
                    path: cdp.clone(),
                }
            })
        } else {
            Ok(())
        }
    }

    /// Returns the approach used to select target chip.
    pub fn resolve_chip(&self, crate_root: &Path) -> TargetSelector {
        // Load the cargo manifest if it is available and parse the meta
        // object.
        let meta = read_metadata(&crate_root).ok();

        // First use command line, then manifest, then default to auto.
        match (&self.chip, meta.map(|m| m.chip).flatten()) {
            (Some(c), _) => c.into(),
            (_, Some(c)) => c.into(),
            _ => TargetSelector::Auto,
        }
    }

    pub fn attach_to_probe(&self) -> Result<Probe, CargoFlashError> {
        // Tries to open the debug probe from the given commandline
        // arguments. This ensures that there is only one probe
        // connected or if multiple probes are found, a single one is
        // specified via the commandline parameters.
        let mut probe = {
            if self.dry_run {
                return Ok(Probe::from_specific_probe(Box::new(FakeProbe::new())));
            }

            // If we got a probe selector as an argument, open the probe
            // matching the selector if possible.
            match &self.probe_selector {
                Some(selector) => {
                    Probe::open(selector.clone()).map_err(CargoFlashError::FailedToOpenProbe)
                }
                None => {
                    // Only automatically select a probe if there is
                    // only a single probe detected.
                    let list = Probe::list_all();
                    if list.len() > 1 {
                        return Err(CargoFlashError::MultipleProbesFound { number: list.len() });
                    }

                    if let Some(info) = list.first() {
                        Probe::open(info).map_err(CargoFlashError::FailedToOpenProbe)
                    } else {
                        Err(CargoFlashError::NoProbesFound)
                    }
                }
            }
        }?;

        // Select protocol and speed
        probe.select_protocol(self.protocol).map_err(|error| {
            CargoFlashError::FailedToSelectProtocol {
                source: error,
                protocol: self.protocol,
            }
        })?;
        let _protocol_speed = if let Some(speed) = self.speed {
            let actual_speed = probe.set_speed(speed).map_err(|error| {
                CargoFlashError::FailedToSelectProtocolSpeed {
                    source: error,
                    speed,
                }
            })?;

            if actual_speed < speed {
                log::warn!(
                    "Unable to use specified speed of {} kHz, actual speed used is {} kHz",
                    speed,
                    actual_speed
                );
            }

            actual_speed
        } else {
            probe.speed_khz()
        };

        Ok(probe)
    }

    pub fn acquire_session(
        &self,
        crate_root: &Path,
        elf_path: &Path,
    ) -> Result<(Session, FlashLoader), CargoFlashError> {
        let _chip = self.resolve_chip(&crate_root);

        let (target_selector, flash_loader) = {
            let target = probe_rs::config::get_target_by_name(self.chip.as_ref().unwrap())
                .map_err(|error| CargoFlashError::ChipNotFound {
                    source: error,
                    name: self.chip.as_ref().unwrap().clone(),
                })?;

            let loader = build_flashloader(&target, &elf_path)?;
            (TargetSelector::Specified(target), Some(loader))
        };

        let probe = self.attach_to_probe()?;

        // Create a new session.
        // If we wanto attach under reset, we do this with a special function call.
        // In this case we assume the target to be known.
        // If we do an attach without a hard reset, we also try to automatically detect the chip at hand to improve the userexperience.
        let session = if self.connect_under_reset {
            probe.attach_under_reset(target_selector)
        } else {
            probe.attach(target_selector)
        }
        .map_err(|error| CargoFlashError::AttachingFailed {
            source: error,
            connect_under_reset: self.connect_under_reset,
        })?;

        Ok((session, flash_loader.unwrap()))
    }

    /// Attaches to target session as specified by [FlashOptions]
    /// parameters.
    pub fn target_session(&self) -> Result<Session, CargoFlashError> {
        let probe = self.attach_to_probe()?;

        let target = match &self.chip {
            Some(chip) => {
                TargetSelector::Specified(probe_rs::config::get_target_by_name(chip).map_err(
                    |error| CargoFlashError::ChipNotFound {
                        source: error,
                        name: self.chip.as_ref().unwrap().clone(),
                    },
                )?)
            }
            None => TargetSelector::Auto,
        };

        if self.connect_under_reset {
            probe.attach_under_reset(target)
        } else {
            probe.attach(target)
        }
        .map_err(|error| CargoFlashError::AttachingFailed {
            source: error,
            connect_under_reset: self.connect_under_reset,
        })
    }
}

/// Builds a new flash loader for the given target and ELF.
/// This will check the ELF for validity and check what pages have to be flashed etc.
fn build_flashloader(target: &Target, path: &Path) -> Result<FlashLoader, CargoFlashError> {
    // Create the flash loader
    let mut loader = FlashLoader::new(target.memory_map.to_vec(), target.source().clone());

    // Add data from the ELF.
    let mut file = File::open(&path).map_err(|error| CargoFlashError::FailedToOpenElf {
        source: error,
        path: format!("{}", path.display()),
    })?;

    // Try and load the ELF data.
    loader
        .load_elf_data(&mut file)
        .map_err(CargoFlashError::FailedToLoadElfData)?;

    Ok(loader)
}
