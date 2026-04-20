//! Backend abstraction for the DAP server.
//!
//! The DAP server historically operated directly against a local
//! [`probe_rs::Session`]. To allow the same debugger implementation to drive a
//! target via the probe-rs RPC layer (over a network connection), the
//! session-level operations the debugger actually needs are captured here in
//! the [`DapBackend`] trait.
//!
//! Two implementations are provided:
//!
//! * [`probe_rs::Session`] (local, blanket impl below).
//! * [`rpc::RpcBackend`], which forwards every operation to a probe-rs RPC
//!   server through a [`crate::rpc::client::RpcClient`].

pub mod rpc;

use std::path::Path;

use probe_rs::{Core, CoreType, Error, Session, Target, flashing::FlashError};

use crate::cmd::dap_server::DebuggerError;
use crate::cmd::dap_server::server::configuration::FlashingConfig;
use crate::rpc::functions::flash::ProgressEvent as WireProgressEvent;
use crate::util::flash::build_loader;

/// Session-level operations used by the DAP server.
///
/// Anything the DAP server needs to do against a "whole target" (as opposed to
/// a single [`Core`]) goes through this trait. The DAP code is written against
/// `SessionData<B: DapBackend>` so it can run against either a local
/// [`Session`] or a remote RPC-backed session implementation.
pub trait DapBackend {
    /// Return the available cores on this target.
    fn list_cores(&self) -> Vec<(usize, CoreType)>;

    /// Return the target description.
    fn target(&self) -> &Target;

    /// Return a handle to the requested core.
    fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error>;
}

impl DapBackend for Session {
    fn list_cores(&self) -> Vec<(usize, CoreType)> {
        Session::list_cores(self)
    }

    fn target(&self) -> &Target {
        Session::target(self)
    }

    fn core(&mut self, core_index: usize) -> Result<Core<'_>, Error> {
        Session::core(self, core_index)
    }
}

/// Extension trait used by the DAP server to flash a binary during `launch`
/// and `restart` handling.
///
/// A dedicated trait allows the [`Session`] path to run the historical
/// synchronous flash while the RPC path issues the build/verify/flash
/// operations over the wire. Progress events are surfaced as the wire-format
/// [`WireProgressEvent`] so the DAP server renders progress uniformly.
pub trait FlashingBackend: DapBackend {
    /// Flash `path_to_elf` to the target, invoking `progress` for every
    /// progress event emitted along the way.
    ///
    /// Implementations MUST respect
    /// [`FlashingConfig::verify_before_flashing`]/
    /// [`FlashingConfig::verify_after_flashing`]/
    /// [`FlashingConfig::restore_unwritten_bytes`]/
    /// [`FlashingConfig::full_chip_erase`].
    async fn flash_binary(
        &mut self,
        path_to_elf: &Path,
        config: &FlashingConfig,
        progress: &mut dyn FnMut(WireProgressEvent),
    ) -> Result<(), DebuggerError>;
}

impl FlashingBackend for Session {
    async fn flash_binary(
        &mut self,
        path_to_elf: &Path,
        config: &FlashingConfig,
        progress: &mut dyn FnMut(WireProgressEvent),
    ) -> Result<(), DebuggerError> {
        use probe_rs::flashing::{DownloadOptions, FileDownloadError, FlashProgress};

        let loader = build_loader(self, path_to_elf, config.format_options.clone(), None)?;

        let mut download_options = DownloadOptions::default();
        download_options.keep_unwritten_bytes = config.restore_unwritten_bytes;
        download_options.do_chip_erase = config.full_chip_erase;
        download_options.verify = config.verify_after_flashing;
        // `FlashProgress` carries a borrow (its lifetime parameter `'a`) so
        // we can pass the caller-provided `&mut dyn FnMut` through without
        // any unsafe.
        download_options.progress = FlashProgress::new(|event| {
            WireProgressEvent::from_library_event(event, &mut *progress);
        });

        let do_flashing = if config.verify_before_flashing {
            match loader.verify(self, &mut download_options.progress) {
                Ok(_) => false,
                Err(FlashError::Verify) => true,
                Err(other) => {
                    return Err(DebuggerError::FileDownload(FileDownloadError::Flash(other)));
                }
            }
        } else {
            true
        };

        if do_flashing {
            loader
                .commit(self, download_options)
                .map_err(FileDownloadError::Flash)
                .map_err(DebuggerError::FileDownload)?;
        }

        Ok(())
    }
}
