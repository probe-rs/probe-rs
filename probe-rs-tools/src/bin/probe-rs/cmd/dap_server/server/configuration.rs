use crate::util::common_options::ProbeOptions;
use crate::util::rtt;
use crate::{cmd::dap_server::DebuggerError, FormatOptions};
use anyhow::{anyhow, Result};
use probe_rs::probe::{DebugProbeSelector, WireProtocol};
use serde::{Deserialize, Serialize};
use std::{env::current_dir, path::PathBuf};

use super::startup::TargetSessionType;

/// Shared options for all session level configuration.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    /// Level of information to be logged to the debugger console (Error, Info or Debug)
    #[serde(default = "default_console_log")]
    pub(crate) console_log_level: Option<ConsoleLog>,

    /// Path to the requested working directory for the debugger
    pub(crate) cwd: Option<PathBuf>,

    /// The debug probe selector associated with the debug probe to use. Use 'list' command to see available probes
    pub(crate) probe: Option<DebugProbeSelector>,

    /// The target to be selected.
    pub(crate) chip: Option<String>,

    /// Path to a custom target description yaml.
    pub(crate) chip_description_path: Option<PathBuf>,

    /// Assert target's reset during connect
    #[serde(default)]
    pub(crate) connect_under_reset: bool,

    /// Protocol speed in kHz
    pub(crate) speed: Option<u32>,

    /// Protocol to use for target connection
    pub(crate) wire_protocol: Option<WireProtocol>,

    ///Allow the session to erase all memory of the chip or reset it to factory default.
    #[serde(default)]
    pub(crate) allow_erase_all: bool,

    /// Flashing configuration
    #[serde(default)]
    pub(crate) flashing_config: FlashingConfig,

    /// Every core on the target has certain configuration.
    ///
    /// NOTE: Although we allow specifying multiple core configurations, this is a work in progress, and probe-rs-debugger currently only supports debugging a single core.
    pub(crate) core_configs: Vec<CoreConfig>,
}

impl SessionConfig {
    /// Since VSCode doesn't do field validation checks for relationships in launch.json request types, check it here.
    pub(crate) fn validate_configuration_option_compatibility(
        &self,
        requested_target_session_type: TargetSessionType,
    ) -> Result<(), DebuggerError> {
        // Disallow flashing if the `attach` request type is used.
        if requested_target_session_type == TargetSessionType::AttachRequest
            && (self.flashing_config.flashing_enabled
                || self.flashing_config.halt_after_reset
                || self.flashing_config.full_chip_erase
                || self.flashing_config.restore_unwritten_bytes)
        {
            let message = "Please do not use any of the `flashing_enabled`, `reset_after_flashing`, halt_after_reset`, `full_chip_erase`, or `restore_unwritten_bytes` options when using `attach` request type.";
            return Err(DebuggerError::Other(anyhow!(message)));
        }
        Ok(())
    }

    /// Ensure all file names are correctly specified and that the files they point to are accessible.
    pub(crate) fn validate_config_files(&mut self) -> Result<(), DebuggerError> {
        // Update the `cwd`.
        self.cwd = self.resolve_cwd()?;

        for target_core_config in &mut self.core_configs {
            // Update the `program_binary` and validate that the file exists.
            target_core_config.program_binary = match get_absolute_path(
                self.cwd.as_ref(),
                target_core_config.program_binary.as_ref(),
            ) {
                Ok(program_binary) => {
                    if !program_binary.is_file() {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid program binary file specified '{}'",
                            program_binary.display()
                        )));
                    }
                    Some(program_binary)
                }
                Err(error) => {
                    return Err(DebuggerError::Other(anyhow!(
                            "Please use the `program-binary` option to specify an executable for this target core. {error:?}"
                        )));
                }
            };
            // Update the `svd_file` and validate that the file exists, or else warn the user and continue.
            target_core_config.svd_file =
                match get_absolute_path(self.cwd.as_ref(), target_core_config.svd_file.as_ref()) {
                    Ok(svd_file) => {
                        if !svd_file.is_file() {
                            tracing::warn!("SVD file {} not found.", svd_file.display());
                            None
                        } else {
                            Some(svd_file)
                        }
                    }
                    Err(error) => {
                        // SVD file is not mandatory.
                        tracing::debug!("SVD file not specified: {:?}", error);
                        None
                    }
                };
        }

        self.chip_description_path =
            match get_absolute_path(self.cwd.as_ref(), self.chip_description_path.as_ref()) {
                Ok(description) => {
                    if !description.is_file() {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid chip description file specified '{}'",
                            description.display()
                        )));
                    }
                    Some(description)
                }
                Err(error) => {
                    // Chip description file is not mandatory.
                    tracing::debug!("Chip description file not specified: {:?}", error);
                    None
                }
            };

        Ok(())
    }

    /// Validate the new given cwd for this process exists, or else update the cwd setting to use the running process' current working directory.
    pub(crate) fn resolve_cwd(&self) -> Result<Option<PathBuf>, DebuggerError> {
        let path = match self.cwd {
            Some(ref temp_path) if temp_path.is_dir() => Some(temp_path.to_path_buf()),
            _ => {
                if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    tracing::error!("Cannot use current working directory. Please check existence and permissions.");
                    None
                }
            }
        };

        Ok(path)
    }

    pub(crate) fn probe_options(&self) -> ProbeOptions {
        ProbeOptions {
            chip: self.chip.clone(),
            chip_description_path: self.chip_description_path.clone(),
            protocol: self.wire_protocol,
            non_interactive: true,
            probe: self.probe.clone(),
            speed: self.speed,
            connect_under_reset: self.connect_under_reset,
            dry_run: false,
            allow_erase_all: self.allow_erase_all,
        }
    }
}

/// If the path to the program to be debugged is relative, we join if with the cwd.
fn get_absolute_path(
    configured_cwd: Option<&PathBuf>,
    os_file_to_validate: Option<&PathBuf>,
) -> Result<PathBuf, DebuggerError> {
    match os_file_to_validate {
        Some(temp_path) => {
            let mut new_path = PathBuf::new();
            if temp_path.is_relative() {
                if let Some(cwd_path) = configured_cwd {
                    new_path.push(cwd_path);
                } else {
                    return Err(DebuggerError::Other(anyhow!(
                        "Invalid value {:?} for `cwd`",
                        configured_cwd
                    )));
                }
            }
            new_path.push(temp_path);
            Ok(new_path)
        }
        None => Err(DebuggerError::Other(anyhow!("Missing value for file."))),
    }
}

/// Configuration options to control flashing.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlashingConfig {
    /// Flash the target before debugging
    #[serde(default)]
    pub(crate) flashing_enabled: bool,

    /// Halt the target after reset
    #[serde(default)]
    pub(crate) halt_after_reset: bool,

    /// Do a full chip erase, versus page-by-page erase
    #[serde(default)]
    pub(crate) full_chip_erase: bool,

    /// Restore erased bytes that will not be rewritten from ELF
    #[serde(default)]
    pub(crate) restore_unwritten_bytes: bool,

    /// [`FormatOptions`] to control the flashing operation, depending on the type of binary ( [`probe_rs::flashing::Format`] ) to be flashed.
    #[serde(default)]
    pub(crate) format_options: FormatOptions,
}

/// Configuration options for all core level configuration.
#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct CoreConfig {
    /// The MCU Core to debug. Default is 0
    #[serde(default)]
    pub(crate) core_index: usize,

    /// Binary to debug as a path. Relative to `cwd`, or fully qualified.
    pub(crate) program_binary: Option<PathBuf>,

    /// CMSIS-SVD file for the target. Relative to `cwd`, or fully qualified.
    pub(crate) svd_file: Option<PathBuf>,

    #[serde(flatten)]
    pub(crate) rtt_config: rtt::RttConfig,
}

fn default_console_log() -> Option<ConsoleLog> {
    Some(ConsoleLog::Console)
}

/// The level of information to be logged to the debugger console.
#[derive(Copy, Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ConsoleLog {
    Console,
    Info,
    Debug,
}

impl std::str::FromStr for ConsoleLog {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "console" => Ok(ConsoleLog::Console),
            "info" => Ok(ConsoleLog::Info),
            "debug" => Ok(ConsoleLog::Debug),
            _ => Err(format!(
                "'{s}' is not a valid console log level. Choose from [console, info, or debug]."
            )),
        }
    }
}
