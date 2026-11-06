//! The target side of semihosting.
//!
//! The server does all target access. It answers what it can itself, and sends the calls that need a
//! host resource to the client. This module does not open files or sockets.

use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    time::Duration,
};

use anyhow::bail;
use probe_rs::{
    Core, HaltReason,
    semihosting::{
        CloseRequest, FileLengthRequest, MAX_TRANSFER_LEN, OpenRequest, ReadRequest, SeekRequest,
        SemihostingCommand, WriteRequest,
    },
};
use probe_rs_rpc::{
    host_calls::{
        FileKind, HostCall, HostReply, HostRequest, HostValue, MAX_CHUNK, MAX_OPEN_FILES, OpenMode,
    },
    monitor::SemihostingEvent,
};

use crate::rpc::{
    HostReplies,
    functions::monitor::MonitorSender,
    utils::run_loop::{HaltAction, HaltHandler},
};

const EBADF: i32 = 9;
const EINVAL: i32 = 22;
const EACCES: i32 = 13;
const EINTR: i32 = 4;
const EMFILE: i32 = 24;
const ENAMETOOLONG: i32 = 36;

#[derive(Clone, Copy)]
enum TargetHandle {
    Stdout,
    Stderr,
    Client { handle: u32, kind: FileKind },
}

/// The handles that the target sees. A handle is its index plus one.
#[derive(Default)]
struct HandleTable(Vec<Option<TargetHandle>>);

impl HandleTable {
    fn get(&self, handle: u32) -> Option<TargetHandle> {
        *self.0.get((handle as usize).checked_sub(1)?)?
    }

    fn is_full(&self) -> bool {
        self.0.iter().flatten().count() >= MAX_OPEN_FILES
    }

    fn add(&mut self, handle: TargetHandle) -> NonZeroU32 {
        let index = self.0.iter().position(Option::is_none).unwrap_or_else(|| {
            self.0.push(None);
            self.0.len() - 1
        });
        self.0[index] = Some(handle);
        NonZeroU32::new(index as u32 + 1).expect("an index plus one is not zero")
    }

    fn remove(&mut self, handle: u32) {
        if let Some(entry) = (handle as usize)
            .checked_sub(1)
            .and_then(|index| self.0.get_mut(index))
        {
            *entry = None;
        }
    }

    /// Removes `handle` if it still leads to the client handle `client`. Another core can close
    /// the handle first, and an open can then reuse it.
    fn remove_client(&mut self, handle: u32, client: u32) {
        if matches!(self.get(handle), Some(TargetHandle::Client { handle, .. }) if handle == client)
        {
            self.remove(handle);
        }
    }
}

/// A call that waits for the client.
enum PendingCall {
    Open {
        request: OpenRequest,
        path: String,
        mode: OpenMode,
    },
    Close {
        request: CloseRequest,
        /// The handle of the target.
        target: u32,
        /// The handle of the client.
        handle: u32,
    },
    Seek {
        request: SeekRequest,
        handle: u32,
    },
    FileLength {
        request: FileLengthRequest,
        handle: u32,
    },
    Read {
        request: ReadRequest,
        handle: u32,
        kind: FileKind,
        filled: u32,
        asked: u32,
    },
    Write {
        request: WriteRequest,
        handle: u32,
        kind: FileKind,
        written: u32,
        sent: u32,
    },
}

/// Handles the semihosting calls of one monitor or test call.
pub struct SemihostingHandler<'a> {
    sender: &'a MonitorSender,
    /// `None` if the client has no file mappings.
    replies: Option<HostReplies>,
    handles: HandleTable,
    errno: i32,
    pending: HashMap<usize, PendingCall>,
    /// The cores with a pending call. The first one sends next, so that a long transfer of one core
    /// cannot starve another.
    queue: VecDeque<usize>,
    /// The call that the client works on, and its core.
    in_flight: Option<(u64, usize)>,
}

impl<'a> SemihostingHandler<'a> {
    pub fn new(sender: &'a MonitorSender, replies: Option<HostReplies>) -> Self {
        Self {
            sender,
            replies,
            handles: HandleTable::default(),
            errno: 0,
            pending: HashMap::new(),
            queue: VecDeque::new(),
            in_flight: None,
        }
    }

    pub fn can_handle(command: SemihostingCommand) -> bool {
        matches!(
            command,
            SemihostingCommand::Open(_)
                | SemihostingCommand::Close(_)
                | SemihostingCommand::WriteConsole(_)
                | SemihostingCommand::Write(_)
                | SemihostingCommand::Read(_)
                | SemihostingCommand::Seek(_)
                | SemihostingCommand::FileLength(_)
                | SemihostingCommand::Remove(_)
                | SemihostingCommand::Rename(_)
                | SemihostingCommand::Errno(_)
        )
    }

    /// Handles a command that [`Self::can_handle`] accepts.
    pub fn on_command<R>(
        &mut self,
        command: SemihostingCommand,
        core: &mut Core<'_>,
    ) -> anyhow::Result<HaltAction<R>> {
        let core_id = core.id();
        let call = match command {
            SemihostingCommand::Open(request) => self.on_open(request, core)?,
            SemihostingCommand::Close(request) => self.on_close(request, core)?,
            SemihostingCommand::Write(request) => self.on_write(request, core)?,
            SemihostingCommand::Read(request) => self.on_read(request)?,
            SemihostingCommand::Seek(request) => self
                .client_handle(request.file_handle())
                .map(|(handle, _)| PendingCall::Seek { request, handle }),
            SemihostingCommand::FileLength(request) => self
                .client_handle(request.file_handle())
                .map(|(handle, _)| PendingCall::FileLength { request, handle }),
            SemihostingCommand::WriteConsole(request) => {
                self.send_output("stdout", request.read(core)?.into_bytes());
                None
            }
            SemihostingCommand::Remove(_) | SemihostingCommand::Rename(_) => {
                tracing::warn!(
                    "The target wanted to remove or rename a file, which is not supported."
                );
                self.errno = EACCES;
                None
            }
            SemihostingCommand::Errno(request) => {
                request.write_errno(core, self.errno)?;
                None
            }
            other => unreachable!("`can_handle` does not accept {other}"),
        };

        Ok(match call {
            Some(call) => {
                self.pending.insert(core_id, call);
                self.queue.push_back(core_id);
                HaltAction::Wait
            }
            None => HaltAction::Resume,
        })
    }

    fn send_output(&self, stream: &str, bytes: Vec<u8>) {
        _ = self
            .sender
            .send_semihosting_event(SemihostingEvent::Output {
                stream: stream.into(),
                bytes,
            });
    }

    /// Returns the client handle of a target handle. Sets the errno if there is none.
    fn client_handle(&mut self, handle: u32) -> Option<(u32, FileKind)> {
        match self.handles.get(handle) {
            Some(TargetHandle::Client { handle, kind }) => Some((handle, kind)),
            _ => {
                self.errno = EBADF;
                None
            }
        }
    }

    fn on_open(
        &mut self,
        request: OpenRequest,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        if request.path_is_too_long() {
            self.errno = ENAMETOOLONG;
            return Ok(None);
        }
        let Some(path) = request.path_utf8(core)? else {
            self.errno = EINVAL;
            return Ok(None);
        };

        if self.handles.is_full() {
            self.errno = EMFILE;
            return Ok(None);
        }

        if path == ":tt" {
            let handle = match request.mode().as_bytes().first() {
                Some(b'w') => TargetHandle::Stdout,
                Some(b'a') => TargetHandle::Stderr,
                _ => {
                    tracing::warn!("The target wanted to read from :tt, which is not supported.");
                    self.errno = EINVAL;
                    return Ok(None);
                }
            };
            let handle = self.handles.add(handle);
            request.respond_with_handle(core, handle)?;
            return Ok(None);
        }

        if self.replies.is_none() {
            tracing::warn!("The target wanted to open {path:?}, but no file is mapped.");
            self.errno = EACCES;
            return Ok(None);
        }

        let Some(mode) = open_mode(request.mode()) else {
            self.errno = EINVAL;
            return Ok(None);
        };

        Ok(Some(PendingCall::Open {
            request,
            path,
            mode,
        }))
    }

    fn on_close(
        &mut self,
        request: CloseRequest,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        let target_handle = request.file_handle();
        Ok(match self.handles.get(target_handle) {
            Some(TargetHandle::Client { handle, .. }) => Some(PendingCall::Close {
                request,
                target: target_handle,
                handle,
            }),
            Some(_) => {
                self.handles.remove(target_handle);
                request.success(core)?;
                None
            }
            None => {
                self.errno = EBADF;
                None
            }
        })
    }

    fn on_write(
        &mut self,
        request: WriteRequest,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        let stream = match self.handles.get(request.file_handle()) {
            Some(TargetHandle::Client { handle, kind }) => {
                return Ok(Some(PendingCall::Write {
                    request,
                    handle,
                    kind,
                    written: 0,
                    sent: 0,
                }));
            }
            Some(TargetHandle::Stdout) => "stdout",
            Some(TargetHandle::Stderr) => "stderr",
            None => {
                self.errno = EBADF;
                return Ok(None);
            }
        };

        let bytes = request.read_chunk(core, 0, MAX_TRANSFER_LEN)?;
        let written = bytes.len() as u32;
        self.send_output(stream, bytes);
        request.finish(core, written)?;
        Ok(None)
    }

    fn on_read(&mut self, request: ReadRequest) -> anyhow::Result<Option<PendingCall>> {
        Ok(self
            .client_handle(request.file_handle())
            .map(|(handle, kind)| PendingCall::Read {
                request,
                handle,
                kind,
                filled: 0,
                asked: 0,
            }))
    }

    /// Returns the core that a reply belongs to. Any other reply is stale, for example from a call
    /// that ended early.
    pub fn reply_core(&self, reply: &HostReply) -> Option<usize> {
        self.in_flight
            .filter(|(id, _)| *id == reply.id)
            .map(|(_, core)| core)
    }

    /// Sends the next call of `core`, if no other call is in flight.
    pub fn continue_pending(&mut self, core: &mut Core<'_>) -> anyhow::Result<()> {
        let core_id = core.id();
        if self.in_flight.is_some() || self.queue.front() != Some(&core_id) {
            return Ok(());
        }
        let Some(call) = self.pending.get_mut(&core_id) else {
            return Ok(());
        };

        let request = match call {
            PendingCall::Open { path, mode, .. } => HostRequest::Open {
                path: path.clone(),
                mode: *mode,
            },
            // The client answers a second close of the same handle with an error.
            PendingCall::Close { handle, .. } => HostRequest::Close { handle: *handle },
            PendingCall::Seek { request, handle } => HostRequest::Seek {
                handle: *handle,
                pos: request.position(),
            },
            PendingCall::FileLength { handle, .. } => HostRequest::FileLength { handle: *handle },
            PendingCall::Read {
                request,
                handle,
                filled,
                asked,
                ..
            } => {
                *asked = (request.bytes_to_read() - *filled).min(MAX_CHUNK);
                HostRequest::Read {
                    handle: *handle,
                    len: *asked,
                }
            }
            PendingCall::Write {
                request,
                handle,
                written,
                sent,
                ..
            } => {
                let bytes = request.read_chunk(core, *written, MAX_CHUNK)?;
                *sent = bytes.len() as u32;
                HostRequest::Write {
                    handle: *handle,
                    bytes,
                }
            }
        };

        let Some(replies) = &self.replies else {
            bail!("A call to the client waits, but the client has no file mappings");
        };
        let id = replies.next_id();
        self.in_flight = Some((id, core_id));
        // A closed connection fires the cancellation token, which ends the run loop.
        _ = self.sender.send_host_call(HostCall { id, request });
        Ok(())
    }

    /// Writes the result of a reply to the target. A multi-chunk transfer stays pending.
    pub fn on_reply<R>(
        &mut self,
        reply: HostReply,
        core: &mut Core<'_>,
    ) -> anyhow::Result<HaltAction<R>> {
        let core_id = core.id();
        self.in_flight = None;
        let Some(call) = self.pending.get_mut(&core_id) else {
            bail!("A reply came for core {core_id}, which has no pending call");
        };

        let mut finished = true;
        match (call, reply.result) {
            (PendingCall::Open { request, .. }, Ok(HostValue::Opened { handle, kind })) => {
                let handle = self.handles.add(TargetHandle::Client { handle, kind });
                request.respond_with_handle(core, handle)?;
            }
            (
                PendingCall::Close {
                    request,
                    target,
                    handle,
                },
                result,
            ) => {
                let request = *request;
                self.handles.remove_client(*target, *handle);
                match result {
                    Ok(HostValue::Done) => request.success(core)?,
                    Ok(_) => bail!("The client sent a wrong reply"),
                    Err(error) => self.errno = error.errno(),
                }
            }
            (PendingCall::Seek { request, .. }, Ok(HostValue::Done)) => request.success(core)?,
            (PendingCall::FileLength { request, .. }, Ok(HostValue::Length(length))) => {
                // A larger value does not fit the return value, which then looks like an error.
                match i32::try_from(length) {
                    Ok(length) => request.write_length(core, length)?,
                    Err(_) => self.errno = EINVAL,
                }
            }
            (
                PendingCall::Read {
                    request,
                    kind,
                    filled,
                    asked,
                    ..
                },
                Ok(HostValue::Read(bytes)),
            ) => {
                let read = bytes.len() as u32;
                if read > *asked {
                    bail!("The client sent more data than the target asked for");
                }
                request.write_chunk_to_target(core, *filled, &bytes)?;
                *filled += read;

                // A stream returns what it has. Another read could block for a long time.
                let more =
                    read == *asked && *filled < request.bytes_to_read() && *kind == FileKind::File;
                if more {
                    finished = false;
                } else {
                    request.finish(core, *filled)?;
                }
            }
            (
                PendingCall::Write {
                    request,
                    kind,
                    written,
                    sent,
                    ..
                },
                Ok(HostValue::Written(count)),
            ) => {
                if count > *sent {
                    bail!("The client wrote more data than the target sent");
                }
                *written += count;

                let more = count == *sent
                    && *written < request.bytes_to_write()
                    && *kind == FileKind::File;
                if more {
                    finished = false;
                } else {
                    request.finish(core, *written)?;
                }
            }
            (call, Err(error)) => {
                self.errno = error.errno();
                finish_with_progress(call, core)?;
            }
            (_, Ok(_)) => bail!("The client sent a wrong reply"),
        }

        if !finished {
            self.queue.rotate_left(1);
            return Ok(HaltAction::Wait);
        }
        self.forget(core_id);
        Ok(HaltAction::Resume)
    }

    /// The call of a core ends without a reply. The core runs after this.
    pub fn abort_pending(&mut self, core: &mut Core<'_>) -> anyhow::Result<()> {
        if let Some(call) = self.pending.get(&core.id()) {
            finish_with_progress(call, core)?;
        }
        self.errno = EINTR;
        self.forget(core.id());
        Ok(())
    }

    /// Forgets the call of a core. Its reply is stale after this.
    pub fn forget(&mut self, core_id: usize) {
        self.pending.remove(&core_id);
        self.queue.retain(|core| *core != core_id);
        if self.in_flight.is_some_and(|(_, core)| core == core_id) {
            self.in_flight = None;
        }
    }

    /// Waits for a reply for at most `timeout`.
    pub fn wait(&mut self, timeout: Duration) -> Option<HostReply> {
        match &self.replies {
            Some(replies) => replies.receiver().recv_timeout(timeout).ok(),
            None => {
                std::thread::sleep(timeout);
                None
            }
        }
    }
}

/// Reports a partial transfer. Otherwise the return value stays -1, from the decode of the command.
fn finish_with_progress(call: &PendingCall, core: &mut Core<'_>) -> anyhow::Result<()> {
    match call {
        PendingCall::Read {
            request, filled, ..
        } if *filled > 0 => request.finish(core, *filled)?,
        PendingCall::Write {
            request, written, ..
        } if *written > 0 => request.finish(core, *written)?,
        _ => {}
    }
    Ok(())
}

fn open_mode(mode: &str) -> Option<OpenMode> {
    Some(match mode {
        "r" | "rb" => OpenMode::Read,
        "r+" | "r+b" => OpenMode::ReadUpdate,
        "w" | "wb" => OpenMode::Write,
        "w+" | "w+b" => OpenMode::WriteUpdate,
        "a" | "ab" => OpenMode::Append,
        "a+" | "a+b" => OpenMode::AppendUpdate,
        _ => return None,
    })
}

/// Connects a [`SemihostingHandler`] to the run loop. `on_halt` handles each halt, and passes
/// semihosting calls that the handler accepts to it.
pub struct HostCallHandler<'a, F> {
    host: SemihostingHandler<'a>,
    on_halt: F,
}

impl<'a, F> HostCallHandler<'a, F> {
    pub fn new<R>(host: SemihostingHandler<'a>, on_halt: F) -> Self
    where
        F: FnMut(
            HaltReason,
            &mut Core<'_>,
            &mut SemihostingHandler<'_>,
        ) -> anyhow::Result<HaltAction<R>>,
    {
        Self { host, on_halt }
    }
}

impl<F, R> HaltHandler for HostCallHandler<'_, F>
where
    F: FnMut(
        HaltReason,
        &mut Core<'_>,
        &mut SemihostingHandler<'_>,
    ) -> anyhow::Result<HaltAction<R>>,
{
    type Output = R;
    type Reply = HostReply;

    fn on_halt(
        &mut self,
        reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<HaltAction<R>> {
        (self.on_halt)(reason, core, &mut self.host)
    }

    fn reply_core(&self, reply: &HostReply) -> Option<usize> {
        self.host.reply_core(reply)
    }

    fn on_reply(&mut self, reply: HostReply, core: &mut Core<'_>) -> anyhow::Result<HaltAction<R>> {
        self.host.on_reply(reply, core)
    }

    fn continue_pending(&mut self, core: &mut Core<'_>) -> anyhow::Result<()> {
        self.host.continue_pending(core)
    }

    fn on_interrupted(&mut self, core_id: usize) {
        self.host.forget(core_id);
    }

    fn abort_pending(&mut self, core: &mut Core<'_>) -> anyhow::Result<()> {
        self.host.abort_pending(core)
    }

    fn wait(&mut self, timeout: Duration) -> Option<HostReply> {
        self.host.wait(timeout)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const CLIENT: TargetHandle = TargetHandle::Client {
        handle: 7,
        kind: FileKind::File,
    };

    #[test]
    fn handle_zero_is_invalid() {
        let mut table = HandleTable::default();
        table.add(CLIENT);

        assert!(table.get(0).is_none());
        table.remove(0);
        assert!(table.get(1).is_some());
    }

    /// Two cores can close the same handle, and an open can reuse it before the second close ends.
    #[test]
    fn a_late_close_keeps_a_reused_handle() {
        let mut table = HandleTable::default();
        let handle = table.add(CLIENT).get();
        table.remove_client(handle, 7);
        let reopened = TargetHandle::Client {
            handle: 8,
            kind: FileKind::File,
        };
        assert_eq!(table.add(reopened).get(), handle);

        table.remove_client(handle, 7);

        assert!(table.get(handle).is_some());
    }

    #[test]
    fn a_closed_target_handle_can_be_used_again() {
        let mut table = HandleTable::default();
        assert_eq!(table.add(TargetHandle::Stdout).get(), 1);
        assert_eq!(table.add(TargetHandle::Stderr).get(), 2);

        table.remove(1);
        assert!(table.get(1).is_none());
        assert_eq!(table.add(CLIENT).get(), 1);
    }

    #[test]
    fn the_table_is_limited() {
        let mut table = HandleTable::default();
        for _ in 0..MAX_OPEN_FILES {
            assert!(!table.is_full());
            table.add(CLIENT);
        }
        assert!(table.is_full());
    }

    #[test]
    fn an_unknown_mode_has_no_value() {
        assert_eq!(open_mode("r+b"), Some(OpenMode::ReadUpdate));
        assert_eq!(open_mode("unknown"), None);
    }
}
