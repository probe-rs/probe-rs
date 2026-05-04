//! Materialization of client-uploaded file content into a session-scoped temporary directory.
//!
//! When the DAP server runs on a different machine from the VSCode client (i.e. the
//! `remote_server_mode` flag is enabled in the [`super::configuration::SessionConfig`]), the
//! client cannot pass filesystem paths the server can open. Instead, the client base64-encodes
//! file bytes alongside each path-bearing field in the launch configuration (e.g. `program_binary`
//! is paired with `program_binary_data`). This module decodes those bytes and persists them to a
//! temporary directory that lives for the duration of the debug session, so the rest of the server
//! code can continue treating them as ordinary on-disk files.
//!
//! ## Lifecycle
//!
//! A new [`UploadedFiles`] instance is created at the start of each remote-mode debug session and
//! is dropped (which removes the temporary directory and all files within) at the end of that
//! session — *not* at the end of the server process. In TCP multi-session mode this means each
//! incoming client connection gets its own clean scratch area, and one client's uploaded firmware
//! never lingers on disk while another client is connected.
//!
//! ## Naming
//!
//! Materialized files are named `<role>-<basename>` where `<role>` is a stable per-field
//! identifier supplied by the caller (e.g. `chip-description`, `core-0-program-binary`,
//! `core-0-svd`) and `<basename>` is the file name from the client's original path (so the temp
//! file remains recognizable in log messages and tools like `ls /tmp/probe-rs-dap-*`). The role
//! string is responsible for guaranteeing uniqueness within a session.

use anyhow::anyhow;
use base64::{Engine as _, engine::general_purpose as base64_engine};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

use crate::cmd::dap_server::DebuggerError;

/// A scratch directory holding files uploaded by the DAP client at session start.
///
/// Files are written under a unique sub-directory in the OS temporary directory, prefixed with
/// `probe-rs-dap-`. The directory and its contents are removed automatically when this struct is
/// dropped, which the [`super::debugger::Debugger`] arranges to happen at the end of each remote
/// debug session.
pub(crate) struct UploadedFiles {
    /// The owned temporary directory. Held by value so that it is removed on `Drop`.
    temp_dir: TempDir,
}

impl UploadedFiles {
    /// Create a new uploaded-files area under the OS temp directory.
    ///
    /// The path of the new directory is logged at `INFO` level so a user inspecting the running
    /// session can grep for it (e.g. to verify what was actually flashed).
    pub(crate) fn new() -> Result<Self, DebuggerError> {
        let temp_dir = tempfile::Builder::new()
            .prefix("probe-rs-dap-")
            .tempdir()
            .map_err(|err| {
                DebuggerError::Other(anyhow!(
                    "Could not create temporary directory for client-uploaded files: {err}"
                ))
            })?;
        tracing::info!(
            "Created temporary directory for client-uploaded files: {}",
            temp_dir.path().display()
        );
        Ok(Self { temp_dir })
    }

    /// Decode the supplied base64 payload and write it to a fresh file in the temporary directory,
    /// returning the absolute path to the materialized file.
    ///
    /// `role` is a stable per-field identifier (e.g. `chip-description`,
    /// `core-0-program-binary`, `core-0-svd`) used as the filename prefix. It is the caller's
    /// responsibility to choose role strings that are unique within a session — calling
    /// `materialize` twice with the same `role` will silently overwrite the earlier file.
    ///
    /// `client_path_hint` is used only as a hint to derive a meaningful basename for the
    /// materialized file (so log messages, RTT scan errors, etc. remain recognizable). It is
    /// never opened or stat'd on the server.
    ///
    /// FUTURE: For very large payloads the in-band base64 encoding may be replaced by a chunked
    /// custom DAP request that streams bytes prior to the launch response. The signature of this
    /// method is intentionally narrow so callers do not need to change when that happens.
    pub(crate) fn materialize(
        &mut self,
        role: &str,
        client_path_hint: &Path,
        data_base64: &str,
    ) -> Result<PathBuf, DebuggerError> {
        let bytes = base64_engine::STANDARD.decode(data_base64).map_err(|err| {
            DebuggerError::Other(anyhow!(
                "Could not decode base64 for client-uploaded file (role `{role}`, originally `{}`): {err}",
                client_path_hint.display()
            ))
        })?;

        // Compose `<role>-<basename>` for self-documenting temp dir listings. Falling back to
        // just `<role>` when the client path has no usable file name (rare).
        let basename = client_path_hint
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .filter(|name| !name.is_empty());
        let dest = match basename {
            Some(basename) => self.temp_dir.path().join(format!("{role}-{basename}")),
            None => self.temp_dir.path().join(role),
        };

        std::fs::write(&dest, &bytes).map_err(|err| {
            DebuggerError::Other(anyhow!(
                "Could not write client-uploaded file (role `{role}`, originally `{}`) to temporary location `{}`: {err}",
                client_path_hint.display(),
                dest.display()
            ))
        })?;

        tracing::info!(
            "Materialized client-uploaded file (role `{role}`, originally `{}`, {} bytes) to `{}`",
            client_path_hint.display(),
            bytes.len(),
            dest.display()
        );
        Ok(dest)
    }
}
