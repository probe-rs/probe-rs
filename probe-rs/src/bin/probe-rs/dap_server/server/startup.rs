use super::debugger::{DebugSessionStatus, Debugger};
use crate::dap_server::debug_adapter::{dap::adapter::*, protocol::DapAdapter};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
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
    port: u16,
    vscode: bool,
    log_info_message: &str,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset);

    log_to_console_and_tracing("Starting as a DAP Protocol server");

    let addr = std::net::SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), port);

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

                match debugger.debug_session(debug_adapter, log_info_message) {
                    Err(error) => {
                        tracing::error!("probe-rs-debugger session ended: {}", error);
                    }
                    Ok(DebugSessionStatus::Terminate) => {
                        log_to_console_and_tracing(&format!("....Closing session from  :{addr}"));
                    }
                    Ok(DebugSessionStatus::Continue) | Ok(DebugSessionStatus::Restart(_)) => {
                        tracing::error!("probe-rs-debugger enountered unexpected `DebuggerStatus` in debug() execution. Please report this as a bug.");
                    }
                }
                // Terminate this process if it was started by VSCode
                if vscode {
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

/// All eprintln! messages are picked up by the VSCode extension and displayed in the debug console. We send these to stderr, in addition to logging them, so that they will show up, irrespective of the RUST_LOG level filters.
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
