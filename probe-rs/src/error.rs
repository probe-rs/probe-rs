use crate::DebugProbeError;
use thiserror::Error;
use crate::config::registry::RegistryError;

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
    #[error("Chip does not exist: {0:?}")]
    ChipNotFound(#[from] RegistryError),
}

impl Error {
    pub fn architecture_specific(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ArchitectureSpecific(Box::new(e))
    }
}
