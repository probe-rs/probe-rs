use crate::cmd::gdb_server::target::utils::copy_to_buf;

use super::RuntimeTarget;

use gdbstub::target::ext::thread_extra_info::ThreadExtraInfo;

impl ThreadExtraInfo for RuntimeTarget {
    fn thread_extra_info(
        &self,
        tid: gdbstub::common::Tid,
        buf: &mut [u8],
    ) -> Result<usize, Self::Error> {
        let name = self
            .cores
            .iter()
            .find(|c| c.index + 1 == tid.get())
            .map(|c| c.name.as_str())
            .unwrap_or("unknown");

        Ok(copy_to_buf(name.as_bytes(), buf))
    }
}
