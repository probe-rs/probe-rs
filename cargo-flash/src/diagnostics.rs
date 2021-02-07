/// Error handling
use colored::*;
use std::fmt::{Display, Write};

use bytesize::ByteSize;

use probe_rs::{
    config::MemoryRegion,
    config::{RegistryError, TargetDescriptionSource},
    flashing::{FileDownloadError, FlashError},
    Error, Target,
};
use probe_rs_cli_util::ArtifactError;

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
    let hints: Vec<String> = match &error {
        CargoFlashError::NoProbesFound => vec![
            "If you are on Linux, you most likely need to install the udev rules for your probe.\nSee https://probe.rs/guide/2_probes/udev/ if you do not know how to install them.".into(),
            "If you are on Windows, make sure to install the correct driver. For J-Link usage you will need the https://zadig.akeo.ie/ driver.".into(),
            "For a guide on how to set up your probes, see https://probe.rs/guide/2_probes/.".into(),
        ],
        CargoFlashError::FailedToReadFamilies(_e) => vec![],
        CargoFlashError::FailedToOpenElf { source, path } => match source.kind() {
            std::io::ErrorKind::NotFound => vec![
                format!("Make sure the path '{}' is the correct location of your ELF binary.", path)
            ],
            _ => vec![]
        },
        CargoFlashError::FailedToLoadElfData(e) => match e {
            FileDownloadError::NoLoadableSegments => vec![
                "Please make sure your linker script is correct and not missing at all.".into(),
                "If you are working with Rust, check your `.cargo/config.toml`? If you are new to the rust-embedded ecosystem, please head over to https://github.com/rust-embedded/cortex-m-quickstart.".into()
            ],
            _ => vec![
                "Make sure you are compiling for the correct architecture of your chip.".into()
            ],
        },
        CargoFlashError::FailedToOpenProbe(_e) => vec![
            "This could be a permission issue. Check our guide on how to make all probes work properly on your system: https://probe.rs/guide/2_probes/.".into()
        ],
        CargoFlashError::MultipleProbesFound { .. } => vec![
            "You can select a probe with the `--probe` argument. See `--help` for how to use it.".into()
        ],
        CargoFlashError::FlashingFailed { source, target, target_spec, .. } => generate_flash_error_hints(source, target, target_spec),
        CargoFlashError::FailedChipDescriptionParsing { .. } => vec![],
        CargoFlashError::FailedToChangeWorkingDirectory { .. } => vec![],
        CargoFlashError::FailedToBuildExternalCargoProject { source, path } => match source {
            ArtifactError::NoArtifacts => vec![
                "Use '--example' to specify an example to flash.".into(),
                "Use '--package' to specify which package to flash in a workspace.".into(),
            ],
            ArtifactError::MultipleArtifacts => vec![
                "Use '--bin' to specify which binary to flash.".into(),
            ],
            ArtifactError::CargoBuild(code) => match code {
                Some(101) => vec![
                    "'cargo build' was not successful. Have a look at the error output above.".into(),
                    format!("Make sure '{}' is indeed a cargo project with a Cargo.toml in it.", path),
                ],
                _ => vec![]
            },
            _ => vec![],
        },
        CargoFlashError::FailedToBuildCargoProject(e) => match e {
            ArtifactError::NoArtifacts => vec![
                "Use '--example' to specify an example to flash.".into(),
                "Use '--package' to specify which package to flash in a workspace.".into(),
            ],
            ArtifactError::MultipleArtifacts => vec![
                "Use '--bin' to specify which binary to flash.".into(),
            ],
            ArtifactError::CargoBuild(code) => match code {
                Some(101) => vec![
                    "'cargo build' was not successful. Have a look at the error output above.".into(),
                    "Make sure the working directory you selected is indeed a cargo project with a Cargo.toml in it.".into()
                ],
                _ => vec![]
            },
            _ => vec![],
        },
        CargoFlashError::ChipNotFound { source, .. } => match source {
            RegistryError::ChipNotFound(_) => vec![
                "Did you spell the name of your chip correctly? Capitalization does not matter."
                    .into(),
                "Maybe your chip is not supported yet. You could add it your self with our tool here: https://github.com/probe-rs/target-gen.".into(),
                "You can list all the available chips by passing the `--list-chips` argument.".into(),
            ],
            _ => vec![],
        },
        CargoFlashError::FailedToSelectProtocol { .. } => vec![],
        CargoFlashError::FailedToSelectProtocolSpeed { speed, .. } => vec![
            format!("Try specifying a speed lower than {} kHz", speed)
        ],
        CargoFlashError::AttachingFailed { source, connect_under_reset } => match source {
            Error::ChipNotFound(RegistryError::ChipAutodetectFailed) => vec![
                "Try specifying your chip with the `--chip` argument.".into(),
                "You can list all the available chips by passing the `--list-chips` argument.".into(),
            ],
            _ => if !connect_under_reset {
                vec![
                    "A hard reset during attaching might help. This will reset the entire chip. Run with `--connect-under-reset` to enable this feature.".into()
                ]
            } else {
                vec![]
            },
        },
        CargoFlashError::AttachingToCoreFailed(_e) =>  vec![],
        CargoFlashError::TargetResetFailed(_e) =>  vec![],
        CargoFlashError::TargetResetHaltFailed(_e) => vec![],
    };

    use std::io::Write;

    let mut stderr = std::io::stderr();

    let err_msg = if hints.is_empty() {
        let error = Err::<(), _>(error)
            // .context("An unexpected issue was encountered.")
            .err()
            .unwrap();
        format!("{:?}", error)
    } else {
        format!("{:}", error)
    };

    let _ = write_with_offset(&mut stderr, "Error".red().bold(), &err_msg);

    let _ = writeln!(stderr, "");

    for hint in &hints {
        write_with_offset(&mut stderr, "Hint".blue().bold(), hint);
        let _ = writeln!(stderr, "");
    }

    let _ = stderr.flush();
}

fn generate_flash_error_hints(
    error: &FlashError,
    target: &Target,
    target_spec: &Option<String>,
) -> Vec<String> {
    match error {
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
