use super::RuntimeTarget;
use probe_rs::Error;

use gdbstub::target::{TargetError, TargetResult};

pub(crate) trait GdbErrorExt<T> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>>;

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>>;
}

impl<T> GdbErrorExt<T> for Result<T, Error> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget<'static>> {
        #[cold]
        fn convert_err(e: Error) -> TargetError<anyhow::Error> {
            match e {
                // A core that is not enabled yet (e.g. a secondary core still held in
                // reset by firmware) is not a fatal protocol error: report EIO so GDB
                // can keep the session alive and access to other cores keeps working.
                Error::CoreDisabled(index) => {
                    tracing::debug!("Core {index} is not enabled");
                    // EIO
                    TargetError::Errno(122)
                }
                e => TargetError::Fatal(e.into()),
            }
        }
        self.map_err(convert_err)
    }

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget<'static>> {
        #[cold]
        fn convert_err(e: Error) -> TargetError<anyhow::Error> {
            match e {
                Error::Arm(error) => {
                    tracing::debug!("Error: {error:#}");
                    // EIO
                    TargetError::Errno(122)
                }
                Error::Riscv(error) => {
                    tracing::debug!("Error: {error:#}");
                    // EIO
                    TargetError::Errno(122)
                }
                Error::Xtensa(error) => {
                    tracing::debug!("Error: {error:#}");
                    // EIO
                    TargetError::Errno(122)
                }
                e => TargetError::Fatal(e.into()),
            }
        }
        self.map_err(convert_err)
    }
}
