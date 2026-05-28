use crate::util::common_options::ProbeOptions;
use crate::util::rtt;
use crate::{FormatOptions, cmd::dap_server::DebuggerError};
use anyhow::{Result, anyhow};
use probe_rs::probe::{DebugProbeSelector, WireProtocol};
use serde::{Deserialize, Serialize};
use std::{env::current_dir, path::PathBuf};

use super::startup::TargetSessionType;
use super::uploaded_files::UploadedFiles;

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

    /// Base64-encoded contents of `chip_description_path`, supplied by the DAP client when
    /// `remote_server_mode` is enabled. If present, the bytes are materialized to a
    /// session-scoped temporary file and `chip_description_path` is rewritten to point at that
    /// temp file before [`SessionConfig::validate_config_files`] runs.
    ///
    /// FUTURE: For very large payloads, this in-band base64 encoding may be replaced by a chunked
    /// custom DAP request that streams bytes prior to the launch response.
    pub(crate) chip_description_data: Option<String>,

    /// Indicates that this DAP server is running on a different machine from the VSCode client.
    ///
    /// When `true`:
    /// - The server expects file content (program binary, SVD file, chip description) to be sent
    ///   inline as base64 alongside the corresponding path fields, rather than being read from
    ///   the server's local filesystem. The bytes are materialized to a session-scoped temporary
    ///   directory and the original path fields are rewritten to point at the materialized files.
    /// - The server emits source paths from DWARF debug information verbatim, without attempting
    ///   to verify their existence on the server. Source resolution becomes the responsibility of
    ///   the VSCode client, which has access to the user's source tree.
    /// - The `cwd` field is treated as a display-only string from the client's filesystem; no
    ///   `is_dir()` check is performed and no fallback to the server's current working directory
    ///   is applied. Relative-path resolution against `cwd` does not apply because all
    ///   client-supplied file paths arrive either pre-resolved (already absolute) or are
    ///   materialized to absolute temp paths.
    ///
    /// The probe-rs VSCode extension sets this flag based on the `launch.json` field of the same
    /// name. Defaults to `false` for backward compatibility with existing local stdio/loopback
    /// launches.
    #[serde(default)]
    pub(crate) remote_server_mode: bool,

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

    /// In `remote_server_mode`, decode any client-supplied file payloads and rewrite the
    /// corresponding path fields to point at the materialized temporary files.
    ///
    /// In local mode (the default), this is a no-op: paths are expected to refer to files on the
    /// server's own filesystem.
    ///
    /// This must be called *before* [`SessionConfig::validate_config_files`], because the
    /// validation step's `is_file()` checks are subsequently performed against the (now-materialized)
    /// temporary paths.
    pub(crate) fn materialize_uploaded_files(
        &mut self,
        uploaded_files: &mut UploadedFiles,
    ) -> Result<(), DebuggerError> {
        if !self.remote_server_mode {
            return Ok(());
        }

        if let Some(data) = self.chip_description_data.take() {
            let hint = self
                .chip_description_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("chip-description.yaml"));
            self.chip_description_path =
                Some(uploaded_files.materialize("chip-description", &hint, &data)?);
        }

        for core_config in &mut self.core_configs {
            let core_index = core_config.core_index;
            if let Some(data) = core_config.program_binary_data.take() {
                let hint = core_config
                    .program_binary
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(format!("core-{core_index}.elf")));
                let role = format!("core-{core_index}-program-binary");
                core_config.program_binary = Some(uploaded_files.materialize(&role, &hint, &data)?);
            }
            if let Some(data) = core_config.svd_file_data.take() {
                let hint = core_config
                    .svd_file
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(format!("core-{core_index}.svd")));
                let role = format!("core-{core_index}-svd");
                core_config.svd_file = Some(uploaded_files.materialize(&role, &hint, &data)?);
            }
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
                Ok(Some(program_binary)) => {
                    if !program_binary.is_file() {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid program binary file specified '{}'",
                            program_binary.display()
                        )));
                    }
                    Some(program_binary)
                }
                Ok(None) => None,
                Err(error) => {
                    return Err(DebuggerError::Other(
                        anyhow!("Could not load program binary.").context(error),
                    ));
                }
            };
            // Update the `svd_file` and validate that the file exists, or else warn the user and continue.
            target_core_config.svd_file =
                match get_absolute_path(self.cwd.as_ref(), target_core_config.svd_file.as_ref()) {
                    Ok(Some(svd_file)) => {
                        if !svd_file.is_file() {
                            tracing::warn!("SVD file {} not found.", svd_file.display());
                            None
                        } else {
                            Some(svd_file)
                        }
                    }
                    Ok(None) => None,
                    Err(error) => {
                        return Err(DebuggerError::Other(
                            anyhow!("Could not load SVD file.").context(error),
                        ));
                    }
                };
        }

        self.chip_description_path =
            match get_absolute_path(self.cwd.as_ref(), self.chip_description_path.as_ref()) {
                Ok(Some(description)) => {
                    if !description.is_file() {
                        return Err(DebuggerError::Other(anyhow!(
                            "Invalid chip description file specified '{}'",
                            description.display()
                        )));
                    }
                    Some(description)
                }
                Ok(None) => None,
                Err(error) => {
                    return Err(DebuggerError::Other(
                        anyhow!("Could not load chip description file.").context(error),
                    ));
                }
            };

        Ok(())
    }

    /// Validate the new given cwd for this process exists, or else update the cwd setting to use the running process' current working directory.
    ///
    /// In `remote_server_mode`, the `cwd` is a path on the client's filesystem and is treated as a
    /// display-only string. We do not perform `is_dir()` validation, and we do not fall back to
    /// the server's own current working directory. Relative-path resolution against `cwd` does
    /// not apply in remote mode because all client-supplied file paths arrive either pre-resolved
    /// (already absolute) or are materialized to absolute temp paths by
    /// [`SessionConfig::materialize_uploaded_files`].
    pub(crate) fn resolve_cwd(&self) -> Result<Option<PathBuf>, DebuggerError> {
        if self.remote_server_mode {
            return Ok(self.cwd.clone());
        }

        let path = match self.cwd {
            Some(ref temp_path) if temp_path.is_dir() => Some(temp_path.to_path_buf()),
            _ => {
                if let Ok(current_dir) = current_dir() {
                    Some(current_dir)
                } else {
                    tracing::error!(
                        "Cannot use current working directory. Please check existence and permissions."
                    );
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
            cycle_power: false,
            dry_run: false,
            allow_erase_all: self.allow_erase_all,
        }
    }
}

/// If the path to the program to be debugged is relative, we join if with the cwd.
fn get_absolute_path(
    configured_cwd: Option<&PathBuf>,
    os_file_to_validate: Option<&PathBuf>,
) -> Result<Option<PathBuf>, DebuggerError> {
    match os_file_to_validate {
        Some(temp_path) => {
            let mut new_path = PathBuf::new();
            if temp_path.is_relative() {
                if let Some(cwd_path) = configured_cwd {
                    new_path.push(cwd_path);
                } else {
                    return Err(DebuggerError::Other(anyhow!(
                        "Invalid value {configured_cwd:?} for `cwd`"
                    )));
                }
            }
            new_path.push(temp_path);
            Ok(Some(new_path))
        }
        None => Ok(None),
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

    /// Verify chip contents before erasing, to prevent unnecessary reprogramming
    #[serde(default)]
    pub(crate) verify_before_flashing: bool,

    /// Do a full chip erase, versus page-by-page erase
    #[serde(default)]
    pub(crate) full_chip_erase: bool,

    /// Restore erased bytes that will not be rewritten from ELF
    #[serde(default)]
    pub(crate) restore_unwritten_bytes: bool,

    /// Verify chip contents after flashing
    #[serde(default)]
    pub(crate) verify_after_flashing: bool,

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

    /// Base64-encoded contents of `program_binary`, supplied by the DAP client when
    /// `remote_server_mode` is enabled. See [`SessionConfig::chip_description_data`] for details.
    pub(crate) program_binary_data: Option<String>,

    /// CMSIS-SVD file for the target. Relative to `cwd`, or fully qualified.
    pub(crate) svd_file: Option<PathBuf>,

    /// Base64-encoded contents of `svd_file`, supplied by the DAP client when
    /// `remote_server_mode` is enabled. See [`SessionConfig::chip_description_data`] for details.
    pub(crate) svd_file_data: Option<String>,

    #[serde(flatten)]
    pub(crate) rtt_config: rtt::RttConfig,

    /// Enable reset vector catch if its supported on the target.
    #[serde(default = "default_true")]
    pub(crate) catch_reset: bool,

    /// Enable hardfault vector catch if its supported on the target.
    #[serde(default = "default_true")]
    pub(crate) catch_hardfault: bool,

    /// Enable SVC vector catch (ARMv7-A/R only).
    #[serde(default = "default_true")]
    pub(crate) catch_svc: bool,

    /// Enable HLT vector catch (ARMv7-A/R only).
    #[serde(default = "default_true")]
    pub(crate) catch_hlt: bool,
}

fn default_console_log() -> Option<ConsoleLog> {
    Some(ConsoleLog::Console)
}

fn default_true() -> bool {
    true
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
