use crate::DebuggerError;
use anyhow::{anyhow, Result};
use probe_rs::{DebugProbeSelector, WireProtocol};
use probe_rs_cli_util::rtt;
use serde::Deserialize;
use std::{env::current_dir, path::PathBuf};

/// Shared options for all session level configuration.
#[derive(Clone, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionConfig {
    /// IP port number to listen for incoming DAP connections, e.g. "50000"
    pub(crate) port: Option<u16>,

    /// Level of information to be logged to the debugger console (Error, Info or Debug )
    #[serde(default = "default_console_log")]
    pub(crate) console_log_level: Option<ConsoleLog>,

    /// Path to the requested working directory for the debugger
    pub(crate) cwd: Option<PathBuf>,

    /// The number associated with the debug probe to use. Use 'list' command to see available probes
    #[serde(alias = "probe")]
    pub(crate) probe_selector: Option<DebugProbeSelector>,

    /// The target to be selected.
    pub(crate) chip: Option<String>,

    /// Assert target's reset during connect
    #[serde(default)]
    pub(crate) connect_under_reset: bool,

    /// Protocol speed in kHz
    pub(crate) speed: Option<u32>,

    /// Protocol to use for target connection
    #[serde(rename = "wire_protocol")]
    pub(crate) protocol: Option<WireProtocol>,

    ///Allow the session to erase all memory of the chip or reset it to factory default.
    #[serde(default)]
    pub(crate) allow_erase_all: bool,

    /// Flashing configuration
    pub(crate) flashing_config: FlashingConfig,

    /// Every core on the target has certain configuration.
    ///
    /// NOTE: Although we allow specifying multiple core configurations, this is a work in progress, and probe-rs-debugger currently only supports debugging a single core.
    pub(crate) core_configs: Vec<CoreConfig>,
}

impl SessionConfig {
    /// Ensure all file names are correctly specified and that the files they point to are accessible.
    pub(crate) fn validate_config_files(&mut self) -> Result<(), DebuggerError> {
        // Update the `cwd`.
        self.cwd = self.validate_and_update_cwd()?;

        for target_core_config in &mut self.core_configs {
            // Update the `program_binary` and validate that the file exists.
            target_core_config.program_binary = match qualify_and_update_os_file_path(
                self.cwd.clone(),
                target_core_config.program_binary.as_ref(),
            ) {
                Ok(program_binary) => {
                    if !program_binary.is_file() {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid program binary file specified '{:?}'",
                            program_binary
                        )));
                    }
                    Some(program_binary)
                }
                Err(error) => {
                    return Err(DebuggerError::Other(anyhow!(
                            "Please use the `program-binary` option to specify an executable for this target core. {:?}", error
                        )));
                }
            };
            // Update the `svd_file` and validate that the file exists.
            // If there is a problem with this file, warn the user and continue with the session.
            target_core_config.svd_file = match qualify_and_update_os_file_path(
                self.cwd.clone(),
                target_core_config.svd_file.as_ref(),
            ) {
                Ok(svd_file) => {
                    if !svd_file.is_file() {
                        log::error!("SVD file {:?} not found.", svd_file);
                        None
                    } else {
                        Some(svd_file)
                    }
                }
                Err(error) => {
                    // SVD file is not mandatory.
                    log::debug!("SVD file not specified: {:?}", &error);
                    None
                }
            };
        }

        Ok(())
    }

    /// Validate the new cwd, or else set it from the environment.
    pub(crate) fn validate_and_update_cwd(&self) -> Result<Option<PathBuf>, DebuggerError> {
        Ok(match &self.cwd {
            Some(temp_path) => {
                if temp_path.is_dir() {
                    Some(temp_path.to_path_buf())
                } else if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    log::error!("Cannot use current working directory. Please check existence and permissions.");
                    None
                }
            }
            None => {
                if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    log::error!("Cannot use current working directory. Please check existence and permissions.");
                    None
                }
            }
        })
    }
}

/// If the path to the program to be debugged is relative, we join if with the cwd.
fn qualify_and_update_os_file_path(
    configured_cwd: Option<PathBuf>,
    os_file_to_validate: Option<&PathBuf>,
) -> Result<PathBuf, DebuggerError> {
    match os_file_to_validate {
        Some(temp_path) => {
            let mut new_path = PathBuf::new();
            if temp_path.is_relative() {
                if let Some(cwd_path) = configured_cwd.clone() {
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
#[derive(Clone, Deserialize, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct FlashingConfig {
    /// Flash the target before debugging
    #[serde(default)]
    pub(crate) flashing_enabled: bool,

    /// Reset the target after flashing
    #[serde(default)]
    pub(crate) reset_after_flashing: bool,

    /// Halt the target after reset
    #[serde(default)]
    pub(crate) halt_after_reset: bool,

    /// Do a full chip erase, versus page-by-page erase
    #[serde(default)]
    pub(crate) full_chip_erase: bool,

    /// Restore erased bytes that will not be rewritten from ELF
    #[serde(default)]
    pub(crate) restore_unwritten_bytes: bool,
}

/// Configuration options for all core level configuration.
#[derive(Clone, Deserialize, Debug, Default)]
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
    Some(ConsoleLog::Error)
}

/// The level of information to be logged to the debugger console. The DAP Client will set appropriate RUST_LOG env for 'launch' configurations,  and will pass the rust log output to the client debug console.
#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum ConsoleLog {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl std::str::FromStr for ConsoleLog {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "error" => Ok(ConsoleLog::Error),
            "warn" => Ok(ConsoleLog::Error),
            "info" => Ok(ConsoleLog::Info),
            "debug" => Ok(ConsoleLog::Debug),
            "trace" => Ok(ConsoleLog::Trace),
            _ => Err(format!(
                "'{}' is not a valid console log level. Choose from [error, warn, info, debug, or trace].",
                s
            )),
        }
    }
}
