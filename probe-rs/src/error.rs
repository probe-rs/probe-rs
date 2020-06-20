use crate::config::RegistryError;
use crate::DebugProbeError;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("An error with the usage of the probe occured")]
    Probe(#[from] DebugProbeError),
    #[error("A core architecture specific error occured")]
    ArchitectureSpecific(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("Probe could not be opened: {0}")]
    UnableToOpenProbe(&'static str),
    #[error("Core {0} does not exist")]
    CoreNotFound(usize),
    #[error("Unable to load specification for chip")]
    ChipNotFound(#[from] RegistryError),
    #[error("This feature requires one of the following architectures: {0:?}")]
    ArchitectureRequired(&'static [&'static str]),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl Error {
    pub fn architecture_specific(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ArchitectureSpecific(Box::new(e))
    }
}
