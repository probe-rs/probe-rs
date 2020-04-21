use crate::SharedOptions;

use probe_rs::{
    architecture::arm::ap::AccessPortError, config::TargetSelector, flashing::FileDownloadError,
    DebugProbeError, Error, Probe, Session,
};

use std::fmt;
use thiserror::Error;

use anyhow::Result;

#[derive(Debug, Error)]
pub enum CliError {
    DebugProbe(
        #[source]
        #[from]
        DebugProbeError,
    ),
    AccessPort(
        #[source]
        #[from]
        AccessPortError,
    ),
    StdIO(
        #[source]
        #[from]
        std::io::Error,
    ),
    FileDownload(
        #[source]
        #[from]
        FileDownloadError,
    ),
    MissingArgument,
    UnableToOpenProbe(Option<&'static str>),
    ProbeRs(
        #[source]
        #[from]
        Error,
    ),
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use CliError::*;

        match self {
            DebugProbe(ref e) => e.fmt(f),
            AccessPort(ref e) => e.fmt(f),
            StdIO(ref e) => e.fmt(f),
            FileDownload(ref e) => e.fmt(f),
            MissingArgument => write!(f, "Command expected more arguments."),
            UnableToOpenProbe(ref details) => match details {
                None => write!(f, "Unable to open probe."),
                Some(details) => write!(f, "Unable to open probe: {}", details),
            },
            ProbeRs(ref e) => e.fmt(f),
        }
    }
}

pub(crate) fn open_probe(index: Option<usize>) -> Result<Probe, CliError> {
    let available_probes = Probe::list_all();

    let device = match index {
        Some(index) => available_probes
            .get(index)
            .ok_or(CliError::UnableToOpenProbe(Some("Unable to open the specified probe. Use the 'list' subcommand to see all available probes.")))?,
        None => {
            // open the default probe, if only one probe was found
            match available_probes.len() {
                0 => {
                    return Err(CliError::UnableToOpenProbe(Some("No probe detected.")));
                }
                1 => &available_probes[0],
                _ =>  {
                    return Err(CliError::UnableToOpenProbe(Some("Multiple probes found. Please specify which probe to use using the -n parameter.")));
                }
            }
        }
    };

    let probe = device.open()?;
    Ok(probe)
}

/// Takes a closure that is handed an `DAPLink` instance and then executed.
/// After the closure is done, the USB device is always closed,
/// even in an error case inside the closure!
pub(crate) fn with_device<F>(shared_options: &SharedOptions, f: F) -> Result<()>
where
    F: FnOnce(Session) -> Result<()>,
{
    let mut probe = open_probe(shared_options.n)?;

    let target_selector = match &shared_options.chip {
        Some(identifier) => identifier.into(),
        None => TargetSelector::Auto,
    };

    if let Some(ref protocol) = shared_options.protocol {
        probe.select_protocol(
            protocol
                .parse()
                .map_err(|_e| CliError::UnableToOpenProbe(Some("Error while parsing protocol")))?,
        )?;
    }

    let session = if shared_options.connect_under_reset {
        probe.attach_under_reset(target_selector)?
    } else {
        probe.attach(target_selector)?
    };

    f(session)
}
