use crate::DebugProbeError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("An error with the usage of the probe occured")]
    Probe(#[source] DebugProbeError),
    #[error("A core architecture specific error occured")]
    ArchitectureSpecific(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("Probe could not be opened: {0}")]
    UnableToOpenProbe(&'static str),
    #[error("Core {0} does not exist")]
    CoreNotFound(usize),
}

impl Error {
    pub fn architecture_specific(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ArchitectureSpecific(Box::new(e))
    }
}
