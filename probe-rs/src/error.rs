//! Error handling
//! 
//! 
use std::error;
use std::fmt;

#[macro_export]
macro_rules! res {
    ($kind:expr, $source:expr) => {
        {
            match source {
                Ok(v) => v,
                Err(e) => Err(Error::new_with_source($kind, Some(e)))?,
            }
        }
    };
    ($source:expr) => {
        {
            match $source {
                Ok(v) => v,
                Err(e) => Err(Error::new_with_source(ErrorKind::from(&e), Some(e)))?,
            }
        }
    };
}

#[macro_export]
macro_rules! err {
    ($kind:expr, $source:expr) => {
        {
            Error::new_with_source(ErrorKind::$kind, Some($source))
        }
    };
    ($kind:expr) => {
        {
            Error::new(ErrorKind::from($kind))
        }
    };
}

pub use crate::{err, res};
pub use ErrorKind::*;

#[derive(Debug)]
pub enum ErrorKind {
    /// Error getting target information from the registry
    Registry,
    // DebugProbeError(DebugProbeError),
    // RomTableError(RomTableError),

    NotFound(NotFoundKind),
    Missing(MissingKind),
    Io,
    Yaml,
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    source: Option<Box<dyn error::Error + Send + Sync + 'static>>,
}

impl Error {
    pub fn new_with_source(kind: ErrorKind, source: Option<impl error::Error + Send + Sync + 'static>) -> Self {
        Self {
            kind,
            source: source.map(|s| Box::new(s) as _),
        }
    }

    pub fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            source: None,
        }
    }
}

impl error::Error for Error {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        self.source.as_ref().map(|x| &**x as _)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
        unimplemented!()
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub enum NotFoundKind {
    Chip,
    Algorithm,
    Core,
}

#[derive(Debug)]
pub enum MissingKind {
    RamRegion,
    FlashRegion,
}

impl From<&std::io::Error> for ErrorKind {
    fn from(_value: &std::io::Error) -> ErrorKind {
        ErrorKind::Io
    }
}

impl From<&serde_yaml::Error> for ErrorKind {
    fn from(_value: &serde_yaml::Error) -> ErrorKind {
        ErrorKind::Yaml
    }
}