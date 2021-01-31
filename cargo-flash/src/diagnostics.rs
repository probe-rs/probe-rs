/// Error handling
use colored::*;
use std::fmt::{Display, Write};

use anyhow::{anyhow, Context};
use bytesize::ByteSize;

use probe_rs::{
    config::MemoryRegion,
    config::{RegistryError, TargetDescriptionSource},
    flashing::{FileDownloadError, FlashError},
    Error, Target,
};

use crate::CargoFlashError;

#[derive(Debug)]
pub struct Diagnostic {
    error: anyhow::Error,

    hints: Vec<String>,
}

impl Diagnostic {
    pub fn render(&self) {
        use std::io::Write;

        let mut stderr = std::io::stderr();

        let err_msg = format!("{:?}", self.error);

        let _ = write_with_offset(&mut stderr, "Error".red().bold(), &err_msg);

        let _ = writeln!(stderr, "");

        for hint in &self.hints {
            write_with_offset(&mut stderr, "Hint".blue().bold(), hint);
            let _ = writeln!(stderr, "");
        }

        let _ = stderr.flush();
    }

    pub fn add_hint(&mut self, hint: impl Into<String>) {
        self.hints.push(hint.into())
    }
}

impl Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl From<anyhow::Error> for Diagnostic {
    fn from(error: anyhow::Error) -> Self {
        Diagnostic {
            error,
            hints: vec![],
        }
    }
}

pub(crate) fn render_diagnostics(error: CargoFlashError) {
    let hints = match &error {
        CargoFlashError::NoProbesFound => vec![
            "If you are on Linux, you most likely need to install the udev rules for your probe.\nSee https://probe.rs/guide/2_probes/udev/ if you do not know how to install them.".into(),
            "If you are on Windows, make sure to install the correct driver. For J-Link usage you will need the https://zadig.akeo.ie/ driver.".into(),
            "For a guide on how to set up your probes, see https://probe.rs/guide/2_probes/.".into(),
        ],
        CargoFlashError::FailedToReadFamilies(_) => vec![],
        CargoFlashError::FailedToOpenElf { source, path } => vec![],
        CargoFlashError::FailedToLoadElfData(_) => vec![
            "Make sure you are compiling for the correct architecture of your chip.".into()
        ],
        CargoFlashError::FailedToOpenProbe(_) => vec![
            "This could be a permission issue. Check our guide on how to make all probes work properly on your system: https://probe.rs/guide/2_probes/."
        ],
        CargoFlashError::MultipleProbesFound { number } => vec![
            "You can select a probe with the `--probe` argument. See `--help` for how to use it."
        ],
        CargoFlashError::FlashingFailed { source, path } => vec![],
        CargoFlashError::FailedChipDescriptionParsing { source, path } => vec![],
        CargoFlashError::FailedToChangeWorkingDirectory { source, path } => vec![],
        CargoFlashError::FailedToBuildExternalCargoProject { source, path } => vec![],
        CargoFlashError::FailedToBuildCargoProject(_) => vec![],
        CargoFlashError::ChipNotFound { source, name } => vec![],
        CargoFlashError::FailedToSelectProtocol { source, protocol } => vec![],
        CargoFlashError::FailedToSelectProtocolSpeed { source, speed } => vec![],
        CargoFlashError::AttachingFailed { source, connect_under_reset } => vec![],
        CargoFlashError::AttachingToCoreFailed(_) => vec![],
        CargoFlashError::TargetResetFailed(_) => vec![],
        CargoFlashError::TargetResetHaltFailed(_) => vec![],
    };

    use std::io::Write;

    let mut stderr = std::io::stderr();

    let err_msg = format!("{:?}", error);

    let _ = write_with_offset(&mut stderr, "Error".red().bold(), &err_msg);

    let _ = writeln!(stderr, "");

    for hint in &hints {
        write_with_offset(&mut stderr, "Hint".blue().bold(), hint);
        let _ = writeln!(stderr, "");
    }

    let _ = stderr.flush();
}

pub fn handle_flash_error(
    err: anyhow::Error,
    target: &Target,
    target_spec: Option<&str>,
) -> Diagnostic {
    // Try to get a probe_rs::Error out of the anyhow error

    let hints = if let Some(err) = err.downcast_ref::<FileDownloadError>() {
        handle_file_download_error(err, target, target_spec)
    } else if let Some(err) = err.downcast_ref::<Error>() {
        handle_probe_rs_error(err)
    } else {
        vec![]
    };

    let mut diagnostic: Diagnostic = err.into();

    diagnostic.hints = hints;

    diagnostic
}

fn handle_probe_rs_error(err: &Error) -> Vec<String> {
    match err {
        Error::ChipNotFound(chip_not_found_error) => match chip_not_found_error {
            RegistryError::ChipNotFound(_) => vec![
                "Did you spell the name of your chip correctly? Capitalization does not matter."
                    .into(),
                "Maybe your chip is not supported yet. You could add it your self with our tool here: https://github.com/probe-rs/target-gen.".into(),
                "You can list all the available chips by passing the `--list-chips` argument.".into(),
            ],
            RegistryError::ChipAutodetectFailed => vec![
                "Try specifying your chip with the `--chip` argument.".into()
            ],
            _ => vec![],
        },
        _ => vec![],
    }
}

/*
Err(err) => {
    let hint = match err {
        probe_rs::Error::ChipNotFound(
            probe_rs::config::RegistryError::ChipAutodetectFailed,
        ) => {
            let autodetection_hint = "Specify a chip using the `--chip` option. \n \
                                            A list of all supported chips can be shown using the `--list-chips` command.";
            Some(autodetection_hint.to_owned())
        }
        _ => {
            let mut buff = String::new();
            let _ = writeln!(buff, "The target seems to be unable to be attached to.");
            let _ = writeln!(buff, "A hard reset during attaching might help. This will reset the entire chip.");
            let _ = writeln!(
                buff,
                "Run with `--connect-under-reset` to enable this feature."
            );
            Some(buff.to_owned())
        }
    };

    let mut diagnostic =
        Diagnostic::from(anyhow!(err).context("Failed attaching to target"));

    if let Some(hint) = hint {
        diagnostic.add_hint(hint);
    }

    return Err(diagnostic);
}

*/

/// Use the --probe argument to select which probe to use.

fn handle_file_download_error(
    err: &FileDownloadError,
    target: &Target,
    target_spec: Option<&str>,
) -> Vec<String> {
    match err {
        FileDownloadError::Flash(flash_error) => match flash_error {
            FlashError::NoSuitableNvm {
                start: _,
                end: _,
                description_source,
            } => {
                if &TargetDescriptionSource::Generic == description_source {
                    return vec![
                    "A generic chip was selected as the target. For flashing, it is necessary to specify a concrete chip.\n\
                    Use `--list-chips` to see all available chips.".to_owned()
                    ];
                }

                let mut hints = Vec::new();

                let mut hint_available_regions = String::new();

                // Show the available flash regions
                let _ = writeln!(
                    hint_available_regions,
                    "The following flash memory is available for the chip '{}':",
                    target.name
                );

                for memory_region in &target.memory_map {
                    match memory_region {
                        MemoryRegion::Ram(_) => {}
                        MemoryRegion::Generic(_) => {}
                        MemoryRegion::Nvm(flash) => {
                            let _ = writeln!(
                                hint_available_regions,
                                "  {:#010x} - {:#010x} ({})",
                                flash.range.start,
                                flash.range.end,
                                ByteSize((flash.range.end - flash.range.start) as u64)
                                    .to_string_as(true)
                            );
                        }
                    }
                }

                hints.push(hint_available_regions);

                if let Some(target_spec) = target_spec {
                    // Check if the chip specification was unique
                    let matching_chips = probe_rs::config::search_chips(target_spec).unwrap();

                    log::info!(
                        "Searching for all chips for spec '{}', found {}",
                        target_spec,
                        matching_chips.len()
                    );

                    if matching_chips.len() > 1 {
                        let mut non_unique_target_hint = format!("The specified chip '{}' did match multiple possible targets. Try to specify your chip more exactly. The following possible targets were found:\n", target_spec);

                        for target in matching_chips {
                            non_unique_target_hint.push_str(&format!("\t{}\n", target));
                        }

                        hints.push(non_unique_target_hint)
                    }
                }

                 hints
            },
            FlashError::EraseFailed { ..} => vec![
                "Perhaps your chip has write protected sectors that need to be cleared?".into(),
                "Perhaps you need the --nmagic linker arg https://github.com/rust-embedded/cortex-m-quickstart/pull/95.".into()
            ],
            _ => vec![],
        },
        FileDownloadError::NoLoadableSegments => vec![
            "Please make sure your linker script is correct and not missing at all.".into(),
            "If you are working with Rust, check your `.cargo/config.toml`? If you are new to the rust-embedded ecosystem, please head over to https://github.com/rust-embedded/cortex-m-quickstart.".into()
        ],
        // Ignore other errors
        _ => vec![],
    }
}

fn write_with_offset(mut output: impl std::io::Write, header: ColoredString, msg: &str) {
    let _ = write!(output, "       {} ", header);

    let mut lines = msg.lines();

    if let Some(first_line) = lines.next() {
        let _ = writeln!(output, "{}", first_line);
    }

    for line in lines {
        let _ = writeln!(output, "            {}", line);
    }
}
