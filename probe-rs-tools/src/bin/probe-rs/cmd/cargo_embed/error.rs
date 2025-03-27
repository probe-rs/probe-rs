use probe_rs::{architecture::arm::ap::AccessPortError, probe::DebugProbeError};
use std::error::Error;
use std::fmt;

#[derive(Debug)]
pub enum DownloadError {
    DebugProbe(DebugProbeError),
    AccessPort(AccessPortError),
    StdIO(std::io::Error),
}

impl Error for DownloadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DebugProbe(e) => Some(e),
            Self::AccessPort(e) => Some(e),
            Self::StdIO(e) => Some(e),
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::DebugProbe(e) => e.fmt(f),
            Self::AccessPort(e) => e.fmt(f),
            Self::StdIO(e) => e.fmt(f),
        }
    }
}

impl From<AccessPortError> for DownloadError {
    fn from(error: AccessPortError) -> Self {
        DownloadError::AccessPort(error)
    }
}

impl From<DebugProbeError> for DownloadError {
    fn from(error: DebugProbeError) -> Self {
        DownloadError::DebugProbe(error)
    }
}

impl From<std::io::Error> for DownloadError {
    fn from(error: std::io::Error) -> Self {
        DownloadError::StdIO(error)
    }
}
