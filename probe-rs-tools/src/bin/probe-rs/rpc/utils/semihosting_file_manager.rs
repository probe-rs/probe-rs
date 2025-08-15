use crate::rpc::functions::monitor::SemihostingEvent;
use probe_rs::{
    Core,
    semihosting::{
        CloseRequest, FileLengthRequest, OpenRequest, ReadRequest, RemoveRequest, RenameRequest,
        SeekRequest, SemihostingCommand, WriteRequest,
    },
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
                | SemihostingCommand::Read(_)
                | SemihostingCommand::Seek(_)
                | SemihostingCommand::FileLength(_)
                | SemihostingCommand::Remove(_)
                | SemihostingCommand::Rename(_)
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
            SemihostingCommand::Read(request) => self.handle_read(core, request),
            SemihostingCommand::Seek(request) => self.handle_seek(core, request),
            SemihostingCommand::FileLength(request) => self.handle_file_length(core, request),
            SemihostingCommand::Remove(request) => self.handle_remove(core, request),
            SemihostingCommand::Rename(request) => self.handle_rename(core, request),

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
        let Some((f, log)) = self.get_file_handle("write to", request.file_handle()) else {
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
        let _ = log;
        if let Some(len) = len {
            request.write_status(core, (buf.len() - len) as i32)?;
        }

        Ok(())
    }

    fn handle_read(&mut self, core: &mut Core<'_>, request: ReadRequest) -> anyhow::Result<()> {
        let Some((f, log)) = self.get_file_handle("read from", request.file_handle()) else {
            return Ok(());
        };

        let _ = (f, core);
        log.not_supported();

        Ok(())
    }

    fn handle_seek(&mut self, core: &mut Core<'_>, request: SeekRequest) -> anyhow::Result<()> {
        let Some((f, log)) = self.get_file_handle("seek in", request.file_handle()) else {
            return Ok(());
        };

        let _ = (f, core);
        log.not_supported();

        Ok(())
    }

    fn handle_file_length(
        &mut self,
        core: &mut Core<'_>,
        request: FileLengthRequest,
    ) -> anyhow::Result<()> {
        let action = "read the file length of";
        let Some((f, log)) = self.get_file_handle(action, request.file_handle()) else {
            return Ok(());
        };

        let _ = (f, core);
        log.not_supported();

        Ok(())
    }

    fn handle_remove(&mut self, core: &mut Core<'_>, request: RemoveRequest) -> anyhow::Result<()> {
        let path = request.path(core)?;

        tracing::warn!(
            "Target wanted to remove file {path}, \
            but probe-rs does not support this operation yet. Continuing..."
        );

        Ok(())
    }

    fn handle_rename(&mut self, core: &mut Core<'_>, request: RenameRequest) -> anyhow::Result<()> {
        let from_path = request.from_path(core)?;
        let to_path = request.to_path(core)?;

        tracing::warn!(
            "Target wanted to rename file {from_path} to {to_path}, \
            but probe-rs does not support this operation yet. Continuing..."
        );

        Ok(())
    }

    fn get_file_handle_entry(&mut self, handle: u32) -> Option<&mut Option<FileHandle>> {
        self.file_handles.get_mut(handle as usize - 1)
    }

    fn take_file_handle(&mut self, handle: u32) -> Option<FileHandle> {
        self.get_file_handle_entry(handle)
            .and_then(|inner| inner.take())
    }

    fn get_file_handle(
        &mut self,
        action: &'static str,
        handle: u32,
    ) -> Option<(&mut FileHandle, FileHandleLog)> {
        let Some(Some(file_handle)) = self.get_file_handle_entry(handle) else {
            tracing::warn!("Target wanted to {action} invalid file handle {handle}. Continuing...");
            return None;
        };

        let variant = match file_handle {
            FileHandle::Stdout => "stdout",
            FileHandle::Stderr => "stderr",
        };

        Some((
            file_handle,
            FileHandleLog {
                action,
                handle,
                variant,
            },
        ))
    }

    fn trim_file_handles(&mut self) {
        while let Some(None) = self.file_handles.last() {
            self.file_handles.pop();
        }
    }
}

struct FileHandleLog {
    action: &'static str,
    handle: u32,
    variant: &'static str,
}

impl FileHandleLog {
    fn not_supported(&self) {
        self.warn("probe-rs does not support this operation")
    }

    fn warn(&self, reason: &str) {
        tracing::warn!(
            "Target wanted to {action} file handle {handle} ({variant}), \
            but {reason}. Continuing...",
            action = self.action,
            handle = self.handle,
            variant = self.variant,
        );
    }
}
