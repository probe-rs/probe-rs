use super::RuntimeTarget;
use probe_rs::Error;

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
            Err(e) => match e {
                GdbStubError::TargetError(te) => Err(te),
                other => Err(anyhow::Error::new(other).into()),
            },
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
            Err(Error::Arm(e)) => {
                log::debug!("Error: {:#}", e);
                // EIO
                Err(TargetError::Errno(122))
            }
            Err(Error::Riscv(e)) => {
                log::debug!("Error: {:#}", e);
                // EIO
                Err(TargetError::Errno(122))
            }
            Err(e) => Err(TargetError::Fatal(e)),
        }
    }
}
