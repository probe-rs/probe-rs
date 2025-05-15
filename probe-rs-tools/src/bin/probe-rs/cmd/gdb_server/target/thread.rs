use super::super::target::utils::copy_to_buf;
use super::RuntimeTarget;

use gdbstub::target::ext::thread_extra_info::ThreadExtraInfo;

impl ThreadExtraInfo for RuntimeTarget<'_> {
    fn thread_extra_info(
        &self,
        tid: gdbstub::common::Tid,
        buf: &mut [u8],
    ) -> Result<usize, Self::Error> {
        pollster::block_on(async move {
            let session = self.session.lock().await;
            let name = &session.target().cores[tid.get() - 1].name;

            Ok(copy_to_buf(name.as_bytes(), buf))
        })
    }
}
