use super::{RuntimeTarget, is_core_disabled};
use probe_rs_rpc_client::ClientError;

use gdbstub::target::{TargetError, TargetResult};

pub(crate) trait GdbErrorExt<T> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget>;

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget>;
}

impl<T> GdbErrorExt<T> for Result<T, ClientError> {
    fn into_target_result(self) -> TargetResult<T, RuntimeTarget> {
        self.map_err(|e| {
            if is_core_disabled(&e) {
                tracing::debug!("Core is not enabled: {e}");
                TargetError::Errno(122)
            } else {
                TargetError::Fatal(e.into())
            }
        })
    }

    fn into_target_result_non_fatal(self) -> TargetResult<T, RuntimeTarget> {
        self.map_err(|e| {
            tracing::debug!("Error: {e:#}");
            TargetError::Errno(122)
        })
    }
}
