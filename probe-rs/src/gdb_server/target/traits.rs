use super::RuntimeTarget;
use crate::Error;

use gdbstub::target::{TargetError, TargetResult};

pub(crate) trait GdbErrorExt<T> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>>;

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>>;
}

impl<T> GdbErrorExt<T> for Result<T, Error> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>> {
        match self {
            Ok(v) => Ok(v),
            Err(e) => Err(TargetError::Fatal(e.into())),
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
            Err(e) => Err(TargetError::Fatal(e.into())),
        }
    }
}
