use crate::config::RegistryError;
use crate::{DebugProbeError, DebugProbeSelector};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("An error with the usage of the probe occured: {0}")]
    Probe(#[from] DebugProbeError),
    #[error("A core architecture specific error occured: {0}")]
    ArchitectureSpecific(#[from] Box<dyn std::error::Error + Send + Sync>),
    #[error("Probe could not be opened: {0}")]
    UnableToOpenProbe(&'static str),
    #[error("Core {0} does not exist")]
    CoreNotFound(usize),
    #[error("Unable to load specification for chip: {0}")]
    ChipNotFound(#[from] RegistryError),
}

impl Error {
    pub fn architecture_specific(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::ArchitectureSpecific(Box::new(e))
    }
}
