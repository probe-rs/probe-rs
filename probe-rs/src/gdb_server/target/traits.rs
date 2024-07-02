use super::RuntimeTarget;
use crate::Error;

use gdbstub::stub::GdbStubError;
use gdbstub::target::{TargetError, TargetResult};

pub(crate) trait ProbeRsErrorExt<T> {
    fn into_error(self) -> Result<T, Error>;
}

impl<T> ProbeRsErrorExt<T> for Result<T, std::io::Error> {
    fn into_error(self) -> Result<T, Error> {
        self.map_err(|e| Error::Other(e.into()))
    }
}

impl<T> ProbeRsErrorExt<T> for Result<T, GdbStubError<Error, std::io::Error>> {
    fn into_error(self) -> Result<T, Error> {
        match self {
            Ok(v) => Ok(v),
            Err(e) if e.is_target_error() => Err(e.into_target_error().unwrap()),
            Err(other) => Err(anyhow::Error::new(other).into()),
        }
    }
}

pub(crate) trait GdbErrorExt<T> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>>;

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>>;
}

impl<T> GdbErrorExt<T> for Result<T, Error> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>> {
        match self {
            Ok(v) => Ok(v),
            Err(e) => Err(TargetError::Fatal(e)),
        }
    }

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>> {
        match self {
            Ok(v) => Ok(v),
            Err(Error::Arm(error)) => {
                tracing::debug!("Error: {error:#}");
                // EIO
                Err(TargetError::Errno(122))
            }
            Err(Error::Riscv(error)) => {
                tracing::debug!("Error: {error:#}");
                // EIO
                Err(TargetError::Errno(122))
            }
            Err(Error::Xtensa(error)) => {
                tracing::debug!("Error: {error:#}");
                // EIO
                Err(TargetError::Errno(122))
            }
            Err(e) => Err(TargetError::Fatal(e)),
        }
    }
}
