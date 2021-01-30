use crate::SharedOptions;

use probe_rs::{
    architecture::arm::ap::AccessPortError, config::TargetSelector, flashing::FileDownloadError,
    DebugProbeError, Error, Probe, Session,
};

use thiserror::Error;

use anyhow::Result;

#[derive(Debug, Error)]
pub enum CliError {
    #[error(transparent)]
    DebugProbe(#[from] DebugProbeError),
    #[error(transparent)]
    AccessPort(#[from] AccessPortError),
    #[error(transparent)]
    StdIO(#[from] std::io::Error),
    #[error(transparent)]
    FileDownload(#[from] FileDownloadError),
    #[error("Command expected more arguments.")]
    MissingArgument,
    #[error("Failed to parse argument '{argument}'.")]
    ArgumentParseError {
        argument_index: usize,
        argument: String,
        source: anyhow::Error,
    },
    #[error("Unable to open probe{}", .0.map(|s| format!(": {}", s)).as_deref().unwrap_or("."))]
    UnableToOpenProbe(Option<&'static str>),
    #[error(transparent)]
    ProbeRs(#[from] Error),
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

    if let Some(protocol) = shared_options.protocol {
        probe.select_protocol(protocol)?;
    }

    if let Some(speed) = shared_options.speed {
        let actual_speed = probe.set_speed(speed)?;

        if actual_speed != speed {
            log::warn!(
                "Protocol speed {} kHz not supported, actual speed is {} kHz",
                speed,
                actual_speed
            );
        }
    }

    let session = if shared_options.connect_under_reset {
        probe.attach_under_reset(target_selector)?
    } else {
        probe.attach(target_selector)?
    };

    f(session)
}
