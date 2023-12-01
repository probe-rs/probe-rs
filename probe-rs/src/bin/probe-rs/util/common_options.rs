#![allow(clippy::needless_doctest_main)]
//! Collection of `#[derive(StructOpt)] struct`s common to programs that
//! extend then functionality of cargo-flash.
//!
//! Example usage:
//! ```no_run
//! use clap::Parser;
//! use crate::util::common_options::FlashOptions;
//!
//! #[derive(clap::Parser)]
//! struct Opts {
//!     #[arg(long = "some-opt")]
//!     opt: String,
//!
//!     #[arg(flatten)]
//!     flash_options: FlashOptions,
//! }
//!
//! fn main() {
//!     let opts = Opts::parse();
//!
//!     opts.flash_options.probe_options.maybe_load_chip_desc().unwrap();
//!
//!     // handle --list-{chips,probes}
//!     if opts.flash_options.early_exit(std::io::stdout()).unwrap() {
//!         return;
//!     }
//!
//!     let target_session = opts.flash_options.probe_options.simple_attach().unwrap();
//!
//!     // ...
//! }
//! ```
use super::ArtifactError;

use std::{fs::File, path::Path, path::PathBuf};

use crate::util::parse_u64;
use clap;
use probe_rs::{
    config::{RegistryError, TargetSelector},
    flashing::{FileDownloadError, FlashError},
    DebugProbeError, DebugProbeSelector, FakeProbe, Lister, Permissions, Probe, Session, Target,
    WireProtocol,
};
use serde::{Deserialize, Serialize};

/// Common options when flashing a target device.
#[derive(Debug, clap::Parser)]
pub struct FlashOptions {
    /// Use this flag to reset and halt (instead of just a reset) the attached core after flashing the target.
    #[arg(long)]
    pub reset_halt: bool,
    /// Use this flag to set the log level.
    ///
    /// Default is `warning`. Possible choices are [error, warning, info, debug, trace].
    #[arg(value_name = "level", long)]
    pub log: Option<log::Level>,
    /// The path to the file to be flashed.
    #[arg(value_name = "path", long)]
    pub path: Option<PathBuf>,
    /// The work directory from which cargo-flash should operate from.
    #[arg(value_name = "directory", long)]
    pub work_dir: Option<PathBuf>,

    #[command(flatten)]
    /// Arguments which are forwarded to 'cargo build'.
    pub cargo_options: CargoOptions,
    #[command(flatten)]
    /// Argument relating to probe/chip selection/configuration.
    pub probe_options: ProbeOptions,
    #[command(flatten)]
    /// Argument relating to probe/chip selection/configuration.
    pub download_options: BinaryDownloadOptions,

    #[command(flatten)]
    pub format_options: crate::FormatOptions,
}

/// Common options when flashing a target device.
#[derive(Debug, clap::Parser)]
pub struct BinaryDownloadOptions {
    #[arg(long)]
    pub disable_progressbars: bool,
    /// Use this flag to disable double-buffering when downloading flash data. If
    /// download fails during programming with timeout errors, try this option.
    #[arg(long)]
    pub disable_double_buffering: bool,
    /// Enable this flag to restore all bytes erased in the sector erase but not overwritten by any page.
    #[arg(long)]
    pub restore_unwritten: bool,
    /// Requests the flash builder to output the layout into the given file in SVG format.
    #[arg(value_name = "filename", long = "flash-layout")]
    pub flash_layout_output_path: Option<String>,
    /// After flashing, read back all the flashed data to verify it has been written correctly.
    #[arg(long)]
    pub verify: bool,
}

/// Supported bit-widths for read/write commands (not every device may support each width).
#[derive(Debug, Copy, Clone, Serialize, Deserialize, clap::ValueEnum)]
pub enum ReadWriteBitWidth {
    /// 8-bit width
    B8 = 8,
    /// 32-bit width
    B32 = 32,
    /// 64-bit width
    B64 = 64,
}

/// Common options for read/write operations to a target device.
#[derive(Debug, clap::Parser)]
pub struct ReadWriteOptions {
    /// Width of the data to read/write.
    #[clap(value_enum, ignore_case = true)]
    pub width: ReadWriteBitWidth,
    /// The address to start from.
    /// Takes an integer as an argument, and can be specified in decimal (16), hexadecimal (0x10) or octal (0o20) format.
    #[clap(value_parser = parse_u64)]
    pub address: u64,
}

/// Common options and logic when interfacing with a [Probe].
#[derive(clap::Parser, Debug)]
pub struct ProbeOptions {
    #[arg(long)]
    pub chip: Option<String>,
    #[arg(value_name = "chip description file path", long)]
    pub chip_description_path: Option<PathBuf>,

    /// Protocol used to connect to chip. Possible options: [swd, jtag]
    #[arg(long, help_heading = "PROBE CONFIGURATION")]
    pub protocol: Option<WireProtocol>,

    /// Use this flag to select a specific probe in the list.
    ///
    /// Use '--probe VID:PID' or '--probe VID:PID:Serial' if you have more than one
    /// probe with the same VID:PID.",
    #[arg(long = "probe", help_heading = "PROBE CONFIGURATION")]
    pub probe_selector: Option<DebugProbeSelector>,
    /// The protocol speed in kHz.
    #[arg(long, help_heading = "PROBE CONFIGURATION")]
    pub speed: Option<u32>,
    /// Use this flag to assert the nreset & ntrst pins during attaching the probe to
    /// the chip.
    #[arg(long)]
    pub connect_under_reset: bool,
    #[arg(long)]
    pub dry_run: bool,
    /// Use this flag to allow all memory, including security keys and 3rd party
    /// firmware, to be erased even when it has read-only protection.
    #[arg(long)]
    pub allow_erase_all: bool,
}

impl ProbeOptions {
    pub fn load(self) -> Result<LoadedProbeOptions, OperationError> {
        LoadedProbeOptions::new(self)
    }

    /// Convenience method that attaches to the specified probe, target,
    /// and target session.
    pub fn simple_attach(
        self,
        lister: &Lister,
    ) -> Result<(Session, LoadedProbeOptions), OperationError> {
        let common_options = self.load()?;

        let target = common_options.get_target_selector()?;
        let probe = common_options.attach_probe(lister)?;
        let session = common_options.attach_session(probe, target)?;

        Ok((session, common_options))
    }
}

/// Common options and logic when interfacing with a [Probe] which already did all pre operation preparation.
#[derive(Debug)]
pub struct LoadedProbeOptions(ProbeOptions);

impl LoadedProbeOptions {
    /// Performs necessary init calls such as loading all chip descriptions
    /// and returns a newtype that ensures initialization.
    pub(crate) fn new(probe_options: ProbeOptions) -> Result<Self, OperationError> {
        let options = Self(probe_options);
        // Load the target description, if given in the cli parameters.
        options.maybe_load_chip_desc()?;
        Ok(options)
    }

    /// Add targets contained in file given by --chip-description-path
    /// to probe-rs registry.
    ///
    /// Note: should be called before [FlashOptions::early_exit] and any other functions in [ProbeOptions].
    fn maybe_load_chip_desc(&self) -> Result<(), OperationError> {
        if let Some(ref cdp) = self.0.chip_description_path {
            let file = File::open(Path::new(cdp)).map_err(|error| {
                OperationError::ChipDescriptionNotFound {
                    source: error,
                    path: cdp.clone(),
                }
            })?;
            probe_rs::config::add_target_from_yaml(file).map_err(|error| {
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
        let target = if let Some(chip_name) = &self.0.chip {
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
    pub fn attach_probe(&self, lister: &Lister) -> Result<Probe, OperationError> {
        let mut probe = {
            if self.0.dry_run {
                Probe::from_specific_probe(Box::new(FakeProbe::new()));
            }

            // If we got a probe selector as an argument, open the probe
            // matching the selector if possible.
            match &self.0.probe_selector {
                Some(selector) => lister
                    .open(selector)
                    .map_err(OperationError::FailedToOpenProbe),
                None => {
                    // Only automatically select a probe if there is
                    // only a single probe detected.
                    let list = lister.list_all();
                    if list.len() > 1 {
                        return Err(OperationError::MultipleProbesFound { number: list.len() });
                    }

                    if let Some(info) = list.first() {
                        lister.open(info).map_err(OperationError::FailedToOpenProbe)
                    } else {
                        Err(OperationError::NoProbesFound)
                    }
                }
            }
        }?;

        if let Some(protocol) = self.0.protocol {
            // Select protocol and speed
            probe.select_protocol(protocol).map_err(|error| {
                OperationError::FailedToSelectProtocol {
                    source: error,
                    protocol,
                }
            })?;
        }

        if let Some(speed) = self.0.speed {
            let _actual_speed = probe.set_speed(speed).map_err(|error| {
                OperationError::FailedToSelectProtocolSpeed {
                    source: error,
                    speed,
                }
            })?;

            // Warn the user if they specified a speed the debug probe does not support
            // and a fitting speed was automatically selected.
            let protocol_speed = probe.speed_khz();
            if let Some(speed) = self.0.speed {
                if protocol_speed < speed {
                    log::warn!(
                        "Unable to use specified speed of {} kHz, actual speed used is {} kHz",
                        speed,
                        protocol_speed
                    );
                }
            }

            log::info!("Protocol speed {} kHz", protocol_speed);
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
        let mut permissions = Permissions::new();
        if self.0.allow_erase_all {
            permissions = permissions.allow_erase_all();
        }

        let session = if self.0.connect_under_reset {
            probe.attach_under_reset(target, permissions)
        } else {
            probe.attach(target, permissions)
        }
        .map_err(|error| OperationError::AttachingFailed {
            source: error,
            connect_under_reset: self.0.connect_under_reset,
        })?;

        Ok(session)
    }

    pub(crate) fn protocol(&self) -> Option<WireProtocol> {
        self.0.protocol
    }

    pub(crate) fn connect_under_reset(&self) -> bool {
        self.0.connect_under_reset
    }

    pub(crate) fn dry_run(&self) -> bool {
        self.0.dry_run
    }

    pub(crate) fn chip(&self) -> Option<String> {
        self.0.chip.clone()
    }
}

impl AsRef<ProbeOptions> for LoadedProbeOptions {
    fn as_ref(&self) -> &ProbeOptions {
        &self.0
    }
}

#[derive(clap::Parser, Debug, Default)]
pub struct CargoOptions {
    #[arg(value_name = "binary", long, hide = true)]
    pub bin: Option<String>,
    #[arg(long, hide = true)]
    pub example: Option<String>,
    #[arg(short, long, hide = true)]
    pub package: Option<String>,
    #[arg(long, hide = true)]
    pub release: bool,
    #[arg(long, hide = true)]
    pub target: Option<String>,
    #[arg(value_name = "PATH", long, hide = true)]
    pub manifest_path: Option<PathBuf>,
    #[arg(long, hide = true)]
    pub no_default_features: bool,
    #[arg(long, hide = true)]
    pub all_features: bool,
    #[arg(long, hide = true)]
    pub features: Vec<String>,
    /// Escape hatch: all args passed after a sentinel `--` end up here,
    /// unprocessed. Used to pass arguments to cargo not declared in
    /// [CargoOptions].
    #[arg(hide = true)]
    pub trailing_opts: Vec<String>,
}

impl CargoOptions {
    /// Generates a suitable help string to append to your program's
    /// --help. Example usage:
    /// ```no_run
    /// use crate::util::common_options::{FlashOptions, CargoOptions};
    /// use crate::util::clap::{Parser, CommandFactory, FromArgMatches};
    ///
    /// let help_message = CargoOptions::help_message("cargo flash");
    ///
    /// let matches = FlashOptions::command()
    ///     .bin_name("cargo flash")
    ///     .after_help(&help_message)
    ///     .get_matches_from(std::env::args());
    /// let opts = FlashOptions::from_arg_matches(&matches);
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

    Additionally, all options passed after a sentinel '--'
    are also forwarded.

    For example, if you run the command '{bin} --release -- \
    --some-cargo-flag', this means that 'cargo build \
    --release --some-cargo-flag' will be called.
"#
        )
    }

    /// Generates list of arguments to cargo from a `CargoOptions`. For
    /// example, if [CargoOptions::release] is set, resultant list will
    /// contain a `"--release"`.
    pub fn to_cargo_options(&self) -> Vec<String> {
        // Handle known options
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
        maybe_push_str_opt!(&self.target, target);
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

        // handle unknown options (passed after sentinel '--')
        args.append(&mut self.trailing_opts.clone());

        args
    }
}

#[derive(Debug, thiserror::Error)]
pub enum OperationError {
    #[error("No connected probes were found.")]
    NoProbesFound,
    #[error("Failed to open the ELF file '{path}' for flashing.")]
    #[allow(dead_code)]
    FailedToOpenElf {
        #[source]
        source: std::io::Error,
        path: PathBuf,
    },
    #[error("Failed to load the ELF data.")]
    #[allow(dead_code)]
    FailedToLoadElfData(#[source] FileDownloadError),
    #[error("Failed to open the debug probe.")]
    FailedToOpenProbe(#[source] DebugProbeError),
    #[error("{number} probes were found.")]
    MultipleProbesFound { number: usize },
    #[error("The flashing procedure failed for '{path}'.")]
    FlashingFailed {
        #[source]
        source: FlashError,
        target: Box<Target>, // Box to reduce enum size
        target_spec: Option<String>,
        path: PathBuf,
    },
    #[error("Failed to open the chip description '{path}'.")]
    ChipDescriptionNotFound {
        #[source]
        source: std::io::Error,
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
    #[error("The protocol speed could not be set to '{speed}' kHz.")]
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
    #[error("Failed to parse CLI arguments.")]
    CliArgument(#[from] clap::Error),
}

impl From<std::io::Error> for OperationError {
    fn from(e: std::io::Error) -> Self {
        OperationError::IOError(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_cargo_options() {
        assert_eq!(
            CargoOptions {
                bin: Some("foobar".into()),
                example: Some("foobar".into()),
                package: Some("foobar".into()),
                release: true,
                target: Some("foobar".into()),
                manifest_path: Some("/tmp/Cargo.toml".into()),
                no_default_features: true,
                all_features: true,
                features: vec!["feat1".into(), "feat2".into()],
                trailing_opts: vec!["--some-cargo-option".into()],
            }
            .to_cargo_options(),
            [
                "--bin",
                "foobar",
                "--example",
                "foobar",
                "--package",
                "foobar",
                "--release",
                "--target",
                "foobar",
                "--manifest-path",
                "/tmp/Cargo.toml",
                "--no-default-features",
                "--all-features",
                "--features",
                "feat1,feat2",
                "--some-cargo-option",
            ]
        );
    }
}
