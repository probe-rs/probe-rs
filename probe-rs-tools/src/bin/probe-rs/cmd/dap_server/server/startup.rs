use super::debugger::Debugger;
use crate::cmd::dap_server::debug_adapter::{dap::adapter::*, protocol::DapAdapter};
use anyhow::Result;
use probe_rs::probe::list::Lister;
use serde::Deserialize;
use std::{
    fs,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};
use time::UtcOffset;
use tokio::net::TcpListener;

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

pub async fn debug(
    lister: &Lister,
    addr: std::net::SocketAddr,
    single_session: bool,
    log_file: Option<&Path>,
    timestamp_offset: UtcOffset,
) -> Result<()> {
    let mut debugger = Debugger::new(timestamp_offset, log_file)?;

    let old_hook = std::panic::take_hook();
    let logger = debugger.debug_logger.clone();
    std::panic::set_hook(Box::new(move |panic_info| {
        // Flush logs before printing panic.
        _ = logger.flush();
        old_hook(panic_info);
    }));

    loop {
        let listener = TcpListener::bind(addr).await?;

        debugger
            .debug_logger
            .log_to_console(&format!("Listening for requests on port {}", addr.port()))?;

        if !single_session {
            // When running as a server from the command line, we want startup logs to go to the stderr.
            debugger.debug_logger.flush().await?;
        }

        match listener.accept().await {
            Ok((socket, addr)) => {
                debugger
                    .debug_logger
                    .log_to_console(&format!("Starting debug session from: {addr}"))?;

                let (read, write) = socket.into_split();

                let dap_adapter = DapAdapter::new(read, write)?;
                let mut debug_adapter = DebugAdapter::new(dap_adapter);

                // Flush any pending log messages to the debug adapter Console Log.
                debugger
                    .debug_logger
                    .flush_to_dap(&mut debug_adapter)
                    .await?;

                let end_message = match debugger.debug_session(debug_adapter, lister).await {
                    // We no longer have a reference to the `debug_adapter`, so errors need
                    // special handling to ensure they are displayed to the user.
                    Err(error) => {
                        eprintln!("Session ended with error: {error:?}");
                        format!("Session ended: {error}")
                    }
                    Ok(()) => format!("Closing debug session from: {addr}"),
                };
                debugger.debug_logger.log_to_console(&end_message)?;

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
        debugger.debug_logger.flush().await?;
    }

    debugger
        .debug_logger
        .log_to_console("DAP Protocol server exiting")?;

    debugger.debug_logger.flush().await?;

    Ok(())
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
