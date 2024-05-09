use super::debugger::Debugger;
use crate::cmd::dap_server::debug_adapter::{dap::adapter::*, protocol::DapAdapter};
use anyhow::{Context, Result};
use probe_rs::probe::list::Lister;
use serde::Deserialize;
use std::{
    fs,
    net::TcpListener,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;

#[derive(Copy, Clone, Debug, Deserialize, PartialEq)]
pub(crate) enum TargetSessionType {
    AttachRequest,
    LaunchRequest,
}

impl std::str::FromStr for TargetSessionType {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "attach" => Ok(TargetSessionType::AttachRequest),
            "launch" => Ok(TargetSessionType::LaunchRequest),
            _ => Err(format!(
                "'{s}' is not a valid target session type. Can be either 'attach' or 'launch']."
            )),
        }
    }
}

pub fn debug(
    lister: &Lister,
    addr: std::net::SocketAddr,
    single_session: bool,
    log_info_message: &str,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset);

    log_to_console_and_tracing("Starting as a DAP Protocol server");

    // Tell the user if (and where) RUST_LOG messages are written.
    log_to_console_and_tracing(log_info_message);

    loop {
        let listener = TcpListener::bind(addr)?;

        log_to_console_and_tracing(&format!("Listening for requests on port {}", addr.port()));

        listener.set_nonblocking(false)?;

        match listener.accept() {
            Ok((socket, addr)) => {
                socket.set_nonblocking(true).with_context(|| {
                    format!("Failed to negotiate non-blocking socket with request from :{addr}")
                })?;

                log_to_console_and_tracing(&format!("..Starting session from   :{addr}"));

                let reader = socket
                    .try_clone()
                    .context("Failed to establish a bi-directional Tcp connection.")?;
                let writer = socket;

                let dap_adapter = DapAdapter::new(reader, writer);
                let debug_adapter = DebugAdapter::new(dap_adapter);

                // TODO: If we can find a way to not consume the `debug_adapter` here, we could
                // clean up the error handling in the VSCode extension - to NOT rely on `stderr`.
                match debugger.debug_session(debug_adapter, log_info_message, lister) {
                    Err(error) => {
                        // We no longer have a reference to the `debug_adapter`, so errors need
                        // special handling to ensure they are displayed to the user.
                        // By adding the keyword `ERROR`, we ensure the VSCode extension will
                        // pick up the message from the stderr stream and display it to the user.
                        // If we don't do this, the user will have no indication of why the session ended.
                        log_to_console_and_tracing(&format!(
                            "ERROR: probe-rs-debugger session ended: {error}"
                        ));
                    }
                    Ok(()) => {
                        log_to_console_and_tracing(&format!("....Closing session from  :{addr}"));
                    }
                }
                // Terminate after a single debug session. This is the behavour expected by VSCode
                // if it started probe-rs as a child process.
                if single_session {
                    break;
                }
            }
            Err(error) => {
                tracing::error!(
                    "probe-rs-debugger failed to establish a socket connection. Reason: {:?}",
                    error
                );
            }
        }
    }
    log_to_console_and_tracing("CONSOLE: DAP Protocol server exiting");

    Ok(())
}

/// All eprintln! messages are picked up by the VSCode extension and displayed in the debug console.
/// We send these to stderr, in addition to logging them, so that they will show up, irrespective of
/// the RUST_LOG level filters.
fn log_to_console_and_tracing(message: &str) {
    eprintln!("probe-rs-debug: {}", &message);
    tracing::info!("{}", &message);
}

/// Try to get the timestamp of a file.
///
/// If an error occurs, None is returned.
pub(crate) fn get_file_timestamp(path_to_elf: &Path) -> Option<Duration> {
    fs::metadata(path_to_elf)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
}
