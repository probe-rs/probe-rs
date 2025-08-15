use crate::rpc::functions::monitor::SemihostingEvent;
use probe_rs::{
    Core,
    semihosting::{CloseRequest, OpenRequest, SemihostingCommand, WriteRequest},
};
use std::num::NonZeroU32;

enum FileHandle {
    Stdout,
    Stderr,
}

pub struct SemihostingFileManager {
    file_handles: Vec<Option<FileHandle>>,
}

impl SemihostingFileManager {
    pub fn new() -> Self {
        Self {
            file_handles: vec![],
        }
    }

    pub fn can_handle(other: SemihostingCommand) -> bool {
        matches!(
            other,
            SemihostingCommand::Open(_)
                | SemihostingCommand::Close(_)
                | SemihostingCommand::WriteConsole(_)
                | SemihostingCommand::Write(_)
        )
    }

    pub fn handle(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
        send: &mut impl FnMut(SemihostingEvent),
    ) -> anyhow::Result<()> {
        let mut send_output = |stream: &str, data: String| {
            send(SemihostingEvent::Output {
                stream: stream.into(),
                data,
            });
        };

        match command {
            SemihostingCommand::Open(request) => self.handle_open(core, request),
            SemihostingCommand::Close(request) => self.handle_close(core, request),
            SemihostingCommand::Write(request) => self.handle_write(core, request, send_output),
            SemihostingCommand::WriteConsole(request) => {
                send_output("stdout", request.read(core)?);
                Ok(())
            }

            _ => Ok(()),
        }
    }

    fn open_tt(&self, mode: &str) -> Option<FileHandle> {
        match mode.as_bytes()[0] {
            b'w' => Some(FileHandle::Stdout),
            b'a' => Some(FileHandle::Stderr),
            mode => {
                tracing::warn!(
                    "Target wanted to open file :tt with mode {mode}, \
                    but probe-rs does not support this operation yet. Continuing..."
                );
                None
            }
        }
    }

    fn handle_open(&mut self, core: &mut Core<'_>, request: OpenRequest) -> anyhow::Result<()> {
        let path = request.path(core)?;

        let f = if path == ":tt" {
            self.open_tt(request.mode())
        } else {
            None
        };

        if let Some(f) = f {
            self.file_handles.push(Some(f));
            request.respond_with_handle(
                core,
                NonZeroU32::new(self.file_handles.len() as u32).unwrap(),
            )?;
        }

        Ok(())
    }

    fn handle_close(&mut self, core: &mut Core<'_>, request: CloseRequest) -> anyhow::Result<()> {
        let handle = request.file_handle();
        if self.take_file_handle(handle).is_some() {
            self.trim_file_handles();
            request.success(core)?;
        } else {
            tracing::warn!("Target wanted to close invalid file handle {handle}. Continuing...");
        }

        Ok(())
    }

    fn handle_write(
        &mut self,
        core: &mut Core<'_>,
        request: WriteRequest,
        mut send_output: impl FnMut(&str, String),
    ) -> anyhow::Result<()> {
        let Some(f) = self.get_file_handle("write to", request.file_handle()) else {
            return Ok(());
        };

        let buf = request.read(core)?;
        let len = match f {
            FileHandle::Stdout => {
                send_output("stdout", String::from_utf8_lossy(&buf).into());
                Some(buf.len())
            }
            FileHandle::Stderr => {
                send_output("stderr", String::from_utf8_lossy(&buf).into());
                Some(buf.len())
            }
        };
        if let Some(len) = len {
            request.write_status(core, (buf.len() - len) as i32)?;
        }

        Ok(())
    }

    fn get_file_handle_entry(&mut self, handle: u32) -> Option<&mut Option<FileHandle>> {
        self.file_handles.get_mut(handle as usize - 1)
    }

    fn take_file_handle(&mut self, handle: u32) -> Option<FileHandle> {
        self.get_file_handle_entry(handle)
            .and_then(|inner| inner.take())
    }

    fn get_file_handle(&mut self, action: &'static str, handle: u32) -> Option<&mut FileHandle> {
        let Some(Some(file_handle)) = self.get_file_handle_entry(handle) else {
            tracing::warn!("Target wanted to {action} invalid file handle {handle}. Continuing...");
            return None;
        };

        Some(file_handle)
    }

    fn trim_file_handles(&mut self) {
        while let Some(None) = self.file_handles.last() {
            self.file_handles.pop();
        }
    }
}
