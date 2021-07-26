//! Collection of `#[derive(StructOpt)] struct`s common to programs that
//! extend then functionality of cargo-flash.
//!
//! Example usage:
//! ```text
//! use structopt::StructOpt;
//! use probe_rs_cli_util::common_options::FlashOptions;
//!
//! #[derive(StructOpt)]
//! struct Opts {
//!     #[structopt(long = "some-opt")]
//!     opt: String,
//!
//!     #[structopt(flatten)]
//!     flash_options: FlashOptions,
//! }
//!
//! fn main() {
//!     let opts = Opts::from_iter(std::env::args());
//!
//!     opts.flash_options.probe_options.maybe_load_chip_desc().unwrap();
//!
//!     // handle --list-{chips,probes}
//!     if opts.flash_options.early_exit(std::io::stdout()).unwrap() {
//!         return;
//!     }
//!
//!     let target_session = opts.flash_options.simple_attach().unwarp();
//!
//!     // ...
//! }
//! ```

use crate::ArtifactError;

use std::{fs::File, io::Write, path::Path, path::PathBuf};

use probe_rs::{
    config::{RegistryError, TargetSelector},
    flashing::{FileDownloadError, FlashError, FlashLoader},
    DebugProbeError, DebugProbeSelector, FakeProbe, Probe, Session, Target, WireProtocol,
};
use structopt::StructOpt;

/// Common options when flashing a target device.
#[derive(Debug, StructOpt)]
pub struct FlashOptions {
    #[structopt(short = "V", long = "version")]
    pub version: bool,
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
    pub elf: Option<PathBuf>,
    #[structopt(
        name = "directory",
        long = "work-dir",
        help = "The work directory from which cargo-flash should operate from."
    )]
    pub work_dir: Option<PathBuf>,
    #[structopt(flatten)]
    /// Arguments which are forwarded to 'cargo build'.
    pub cargo_options: CargoOptions,
    #[structopt(flatten)]
    /// Argument relating to probe/chip selection/configuration.
    pub probe_options: ProbeOptions,
}

impl FlashOptions {
    /// Whether calling program should exit prematurely. Effectively
    /// handles --list-{probes,chips}.
    ///
    /// If `Ok(false)` is returned, calling program should continue executing.
    ///
    /// Note: [ProbeOptions::maybe_load_chip_desc] should be called before this function.
    pub fn early_exit(&self, f: impl Write) -> Result<bool, OperationError> {
        if self.list_probes {
            list_connected_probes(f)?;
            return Ok(true);
        }

        if self.list_chips {
            print_families(f)?;
            return Ok(true);
        }

        Ok(false)
    }
}

/// Common options and logic when interfacing with a [Probe].
#[derive(StructOpt, Debug)]
pub struct ProbeOptions {
    #[structopt(name = "chip", long = "chip")]
    pub chip: Option<String>,
    #[structopt(name = "chip description file path", long = "chip-description-path")]
    pub chip_description_path: Option<PathBuf>,
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
    #[structopt(name = "speed", long = "speed", help = "The protocol speed in kHz.")]
    pub speed: Option<u32>,
    #[structopt(long = "dry-run")]
    pub dry_run: bool,
}

impl ProbeOptions {
    /// Add targets contained in file given by --chip-description-path
    /// to probe-rs registery.
    ///
    /// Note: should be called before [FlashOptions::early_exit] and any other functions in [ProbeOptions].
    pub fn maybe_load_chip_desc(&self) -> Result<(), OperationError> {
        if let Some(ref cdp) = self.chip_description_path {
            probe_rs::config::add_target_from_yaml(&Path::new(cdp)).map_err(|error| {
                OperationError::FailedChipDescriptionParsing {
                    source: error,
                    path: cdp.clone(),
                }
            })
        } else {
            Ok(())
        }
    }

    /// Resolves a resultant target selector from passed [ProbeOptions].
    pub fn get_target_selector(&self) -> Result<TargetSelector, OperationError> {
        let target = if let Some(chip_name) = &self.chip {
            let target = probe_rs::config::get_target_by_name(chip_name).map_err(|error| {
                OperationError::ChipNotFound {
                    source: error,
                    name: chip_name.clone(),
                }
            })?;

            TargetSelector::Specified(target)
        } else {
            TargetSelector::Auto
        };

        Ok(target)
    }

    /// Attaches to specified probe and configures it.
    pub fn attach_probe(&self) -> Result<Probe, OperationError> {
        let mut probe = {
            if self.dry_run {
                Probe::from_specific_probe(Box::new(FakeProbe::new()));
            }

            // If we got a probe selector as an argument, open the probe
            // matching the selector if possible.
            match &self.probe_selector {
                Some(selector) => {
                    Probe::open(selector.clone()).map_err(OperationError::FailedToOpenProbe)
                }
                None => {
                    // Only automatically select a probe if there is
                    // only a single probe detected.
                    let list = Probe::list_all();
                    if list.len() > 1 {
                        return Err(OperationError::MultipleProbesFound { number: list.len() });
                    }

                    if let Some(info) = list.first() {
                        Probe::open(info).map_err(OperationError::FailedToOpenProbe)
                    } else {
                        Err(OperationError::NoProbesFound)
                    }
                }
            }
        }?;

        // Select protocol and speed
        probe.select_protocol(self.protocol).map_err(|error| {
            OperationError::FailedToSelectProtocol {
                source: error,
                protocol: self.protocol,
            }
        })?;
        if let Some(speed) = self.speed {
            let _actual_speed = probe.set_speed(speed).map_err(|error| {
                OperationError::FailedToSelectProtocolSpeed {
                    source: error,
                    speed,
                }
            })?;
        }

        Ok(probe)
    }

    /// Attaches to target device session. Attaches under reset if
    /// specified by [ProbeOptions::connect_under_reset].
    pub fn attach_session(
        &self,
        probe: Probe,
        target: TargetSelector,
    ) -> Result<Session, OperationError> {
        let session = if self.connect_under_reset {
            probe.attach_under_reset(target)
        } else {
            probe.attach(target)
        }
        .map_err(|error| OperationError::AttachingFailed {
            source: error,
            connect_under_reset: self.connect_under_reset,
        })?;

        Ok(session)
    }

    /// Convenience method that attaches to the specified probe, target,
    /// and target session.
    pub fn simple_attach(&self) -> Result<Session, OperationError> {
        let target = self.get_target_selector()?;
        let probe = self.attach_probe()?;
        let session = self.attach_session(probe, target)?;

        Ok(session)
    }

    /// Builds a new flash loader for the given target and ELF. This
    /// will check the ELF for validity and check what pages have to be
    /// flashed etc.
    pub fn build_flashloader(
        &self,
        session: &mut Session,
        elf_path: &Path,
    ) -> Result<FlashLoader, OperationError> {
        let target = session.target();

        // Create the flash loader
        let mut loader = FlashLoader::new(target.memory_map.to_vec(), target.source().clone());

        // Add data from the ELF.
        let mut file = File::open(&elf_path).map_err(|error| OperationError::FailedToOpenElf {
            source: error,
            path: elf_path.to_path_buf(),
        })?;

        // Try and load the ELF data.
        loader
            .load_elf_data(&mut file)
            .map_err(OperationError::FailedToLoadElfData)?;

        Ok(loader)
    }
}

/// Common options used when building artifacts with cargo.
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
    #[structopt(name = "PATH", long = "manifest-path", hidden = true)]
    pub manifest_path: Option<PathBuf>,
    #[structopt(long, hidden = true)]
    pub no_default_features: bool,
    #[structopt(long, hidden = true)]
    pub all_features: bool,
    #[structopt(long, hidden = true)]
    pub features: Vec<String>,
}

impl CargoOptions {
    /// Generates a suitable help string to append to your program's
    /// --help. Example usage:
    /// ```text
    /// let matches = FlashOptions::clap()
    ///     .bin_name("cargo flash")
    ///     .after_help(CargoOptions::help_message("cargo flash").as_str())
    ///     .get_matches_from(&args);
    /// let opts = FlashOptions::from_clap(&matches);
    /// ```
    pub fn help_message(bin: &str) -> String {
        format!(
            r#"
CARGO BUILD OPTIONS:

    The following options are forwarded to 'cargo build':

        --bin
        --example
    -p, --package
        --release
        --target
        --manifest-path
        --no-default-features
        --all-features
        --features

    For example, if you run the command '{} --release',
    this means that 'cargo build --release' will be called.
"#,
            bin
        )
    }

    /// Generates list of arguments to cargo from a `CargoOptions`. For
    /// example, if [CargoOptions::release] is set, resultant list will
    /// contain a `"--release"`.
    pub fn to_cargo_arguments(&self) -> Vec<String> {
        let mut args: Vec<String> = vec![];
        macro_rules! maybe_push_str_opt {
            ($field:expr, $name:expr) => {{
                if let Some(value) = $field {
                    args.push(format!("--{}", stringify!($name)));
                    args.push(value.clone());
                }
            }};
        }

        maybe_push_str_opt!(&self.bin, bin);
        maybe_push_str_opt!(&self.example, example);
        maybe_push_str_opt!(&self.package, package);
        if self.release {
            args.push("--release".to_string());
        }
        maybe_push_str_opt!(&self.bin, bin);
        if let Some(path) = &self.manifest_path {
            args.push("--manifest-path".to_string());
            args.push(path.display().to_string());
        }
        if self.no_default_features {
            args.push("--no-default-features".to_string());
        }
        if self.all_features {
            args.push("--all-features".to_string());
        }
        if !self.features.is_empty() {
            args.push("--features".to_string());
            args.push(self.features.join(","));
        }

        args
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("No connected probes were found.")]
    NoProbesFound,
    #[error("Failed to list the target descriptions.")]
    FailedToReadFamilies(#[source] RegistryError),
    #[error("Failed to open the ELF file '{path}' for flashing.")]
    FailedToOpenElf {
        #[source]
        source: std::io::Error,
        path: PathBuf,
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
        path: PathBuf,
    },
    #[error("Failed to parse the chip description '{path}'.")]
    FailedChipDescriptionParsing {
        #[source]
        source: RegistryError,
        path: PathBuf,
    },
    #[error("Failed to change the working directory to '{path}'.")]
    FailedToChangeWorkingDirectory {
        #[source]
        source: std::io::Error,
        path: PathBuf,
    },
    #[error("Failed to build the cargo project at '{path}'.")]
    FailedToBuildExternalCargoProject {
        #[source]
        source: ArtifactError,
        path: PathBuf,
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
    #[error("probe-rs API was called in the wrong order.")]
    InvalidAPIOrder,
}

impl From<std::io::Error> for OperationError {
    fn from(e: std::io::Error) -> Self {
        OperationError::IOError(e)
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
pub fn print_families(mut f: impl Write) -> Result<(), OperationError> {
    writeln!(f, "Available chips:")?;
    for family in probe_rs::config::families().map_err(OperationError::FailedToReadFamilies)? {
        writeln!(f, "{}", &family.name)?;
        writeln!(f, "    Variants:")?;
        for variant in family.variants() {
            writeln!(f, "        {}", variant.name)?;
        }
    }
    Ok(())
}
