use super::RuntimeTarget;
use probe_rs::Error;

use gdbstub::target::{TargetError, TargetResult};

pub(crate) trait GdbErrorExt<T> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>>;

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>>;
}

impl<T> GdbErrorExt<T> for Result<T, Error> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>> {
        match self {
            Ok(v) => Ok(v),
            // A core that is not enabled yet (e.g. a secondary core still held in
            // reset by firmware) is not a fatal protocol error: report EIO so GDB
            // can keep the session alive and access to other cores keeps working.
            Err(Error::CoreDisabled(index)) => {
                tracing::debug!("Core {index} is not enabled");
                // EIO
                Err(TargetError::Errno(122))
            }
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
