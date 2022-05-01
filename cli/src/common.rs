use probe_rs::{
    architecture::arm::ap::AccessPortError, flashing::FileDownloadError, DebugProbeError, Error,
};

#[derive(Debug, thiserror::Error)]
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
    #[error(transparent)]
    ProbeRs(#[from] Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
