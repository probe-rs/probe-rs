//! The target side of semihosting.
//!
//! The server does all target access. It answers what it can itself, and sends the calls that need a
//! host resource to the client. This module does not open files or sockets.

use std::{
    collections::{HashMap, VecDeque},
    num::NonZeroU32,
    time::{Duration, Instant},
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
        FileKind, HostCall, HostError, HostReply, HostRequest, HostValue, MAX_CHUNK,
        MAX_OPEN_FILES, OpenMode,
    },
    monitor::SemihostingEvent,
};
use tokio_util::sync::CancellationToken;

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

/// The posted writes that the client did not answer yet. The limits keep the client queue short, and
/// limit the data that the target believes written.
const MAX_POSTED_WRITES: usize = 16;
const MAX_POSTED_BYTES: u32 = 512 * 1024;

/// The replies that the client can owe at once: the posted writes and one call in flight.
pub const MAX_OPEN_REPLIES: usize = MAX_POSTED_WRITES + 1;

#[derive(Clone, Copy)]
enum TargetHandle {
    Stdout,
    Stderr,
    Client {
        handle: u32,
        kind: FileKind,
        read_ahead: bool,
    },
}

/// What the run loop gets from [`SemihostingHandler::wait`].
pub enum HostEvent {
    Reply(HostReply),
    /// A posted write finished, so the blocked write of a core can continue.
    Credit(usize),
}

/// The writes that the server sent without waiting. The target has already seen success, so a failure
/// latches on the handle.
#[derive(Default)]
struct PostedWrites {
    /// Call id to client handle and length.
    calls: HashMap<u64, (u32, u32)>,
    bytes: u32,
    failed: HashMap<u32, HostError>,
}

impl PostedWrites {
    fn has_credit(&self) -> bool {
        self.calls.len() < MAX_POSTED_WRITES && self.bytes + MAX_CHUNK <= MAX_POSTED_BYTES
    }

    fn add(&mut self, id: u64, handle: u32, len: u32) {
        self.calls.insert(id, (handle, len));
        self.bytes += len;
    }

    /// Records a reply. Returns `false` if the reply is not for a posted write.
    fn complete(&mut self, reply: &HostReply) -> bool {
        let Some((handle, len)) = self.calls.remove(&reply.id) else {
            return false;
        };
        self.bytes -= len;

        let failure = match &reply.result {
            Ok(HostValue::Written(written)) if *written == len => None,
            Ok(_) => Some(HostError::Other),
            Err(error) => Some(*error),
        };
        if let Some(error) = failure {
            self.failed.entry(handle).or_insert(error);
        }
        true
    }

    fn is_empty(&self) -> bool {
        self.calls.is_empty()
    }
}

/// The data that the client read ahead for one handle, and the file position of its start.
#[derive(Default)]
struct ReadBuffer {
    data: VecDeque<u8>,
    /// A `u64`, because sequential reads can go past 4 GiB.
    position: u64,
}

impl ReadBuffer {
    /// Takes up to `len` bytes.
    fn take(&mut self, len: u32) -> Vec<u8> {
        let len = self.data.len().min(len as usize);
        self.position += len as u64;
        self.data.drain(..len).collect()
    }

    /// Moves to `position` without a call to the client. Returns `false` if the data does not reach it.
    fn seek(&mut self, position: u32) -> bool {
        let Some(skip) = u64::from(position).checked_sub(self.position) else {
            return false;
        };
        if skip > self.data.len() as u64 {
            return false;
        }
        self.data.drain(..skip as usize);
        self.position = position.into();
        true
    }

    /// Replaces the data after a seek on the client.
    fn reset(&mut self, position: u32) {
        self.data.clear();
        self.position = position.into();
    }
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

    /// Stops the read-ahead of the client handle `client`.
    fn stop_read_ahead(&mut self, client: u32) {
        for entry in self.0.iter_mut().flatten() {
            if let TargetHandle::Client {
                handle, read_ahead, ..
            } = entry
                && *handle == client
            {
                *read_ahead = false;
            }
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
        read_ahead: bool,
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
    /// A posted write that waits for credit. It is not in the queue, because it sends no call.
    PostWrite {
        request: WriteRequest,
        handle: u32,
        written: u32,
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
    posted: PostedWrites,
    /// The cores that wait for credit.
    credit_waiters: VecDeque<usize>,
    /// The data that the client read ahead, by client handle.
    read_buffers: HashMap<u32, ReadBuffer>,
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
            posted: PostedWrites::default(),
            credit_waiters: VecDeque::new(),
            read_buffers: HashMap::new(),
        }
    }

    /// Fails a call on a handle with a failed posted write.
    fn take_failure(&mut self, handle: u32) -> bool {
        if let Some(error) = self.posted.failed.remove(&handle) {
            self.errno = error.errno();
            true
        } else {
            false
        }
    }

    /// Sends the chunks of a write without waiting. Returns the call if it runs out of credit.
    fn post_write(
        &mut self,
        request: WriteRequest,
        handle: u32,
        mut written: u32,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        while written < request.bytes_to_write() {
            if !self.posted.has_credit() {
                return Ok(Some(PendingCall::PostWrite {
                    request,
                    handle,
                    written,
                }));
            }

            let Some(replies) = &self.replies else {
                bail!("A write to the client waits, but the client has no file mappings");
            };
            let id = replies.next_id();
            let bytes = request.read_chunk(core, written, MAX_CHUNK)?;
            let len = bytes.len() as u32;
            self.posted.add(id, handle, len);
            _ = self.sender.send_host_call(HostCall {
                id,
                request: HostRequest::Write { handle, bytes },
            });
            written += len;
        }

        request.finish(core, written)?;
        Ok(None)
    }

    /// Queues a call, or makes it wait for credit.
    fn wait_for(&mut self, core_id: usize, call: PendingCall) {
        if matches!(call, PendingCall::PostWrite { .. }) {
            self.credit_waiters.push_back(core_id);
        } else {
            self.queue.push_back(core_id);
        }
        self.pending.insert(core_id, call);
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
            SemihostingCommand::Read(request) => self.on_read(request, core)?,
            SemihostingCommand::Seek(request) => self.on_seek(request, core)?,
            SemihostingCommand::FileLength(request) => self
                .client_handle(request.file_handle())
                .filter(|(handle, _)| !self.take_failure(*handle))
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
                self.wait_for(core_id, call);
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
            Some(TargetHandle::Client { handle, kind, .. }) => Some((handle, kind)),
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
            Some(TargetHandle::Client {
                handle,
                kind,
                read_ahead,
            }) => {
                if self.take_failure(handle) {
                    return Ok(None);
                }
                // The firmware can see the timing of a stream, so only a file gets posted writes. A
                // write to a file that is open for reading fails, and must fail at once.
                if kind == FileKind::File && !read_ahead {
                    return self.post_write(request, handle, 0, core);
                }
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

    fn on_read(
        &mut self,
        request: ReadRequest,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        let Some(TargetHandle::Client {
            handle,
            kind,
            read_ahead,
        }) = self.handles.get(request.file_handle())
        else {
            self.errno = EBADF;
            return Ok(None);
        };
        if self.take_failure(handle) {
            return Ok(None);
        }
        // A read-ahead would grow the buffer with each empty read.
        if request.bytes_to_read() == 0 {
            request.finish(core, 0)?;
            return Ok(None);
        }

        let mut filled = 0;
        if let Some(buffer) = self.read_buffers.get_mut(&handle) {
            let bytes = buffer.take(request.bytes_to_read());
            request.write_chunk_to_target(core, 0, &bytes)?;
            filled = bytes.len() as u32;
        }
        if filled > 0 && filled == request.bytes_to_read() {
            request.finish(core, filled)?;
            return Ok(None);
        }

        Ok(Some(PendingCall::Read {
            request,
            handle,
            kind,
            read_ahead,
            filled,
            asked: 0,
        }))
    }

    fn on_seek(
        &mut self,
        request: SeekRequest,
        core: &mut Core<'_>,
    ) -> anyhow::Result<Option<PendingCall>> {
        let Some((handle, _)) = self.client_handle(request.file_handle()) else {
            return Ok(None);
        };
        if self.take_failure(handle) {
            return Ok(None);
        }

        if let Some(buffer) = self.read_buffers.get_mut(&handle)
            && buffer.seek(request.position())
        {
            request.success(core)?;
            return Ok(None);
        }

        Ok(Some(PendingCall::Seek { request, handle }))
    }

    /// Returns the core that a reply belongs to. Any other reply is stale, for example from a call
    /// that ended early.
    pub fn reply_core(&self, event: &HostEvent) -> Option<usize> {
        match event {
            HostEvent::Reply(reply) => self
                .in_flight
                .filter(|(id, _)| *id == reply.id)
                .map(|(_, core)| core),
            HostEvent::Credit(core) => Some(*core),
        }
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
                read_ahead,
                filled,
                asked,
                ..
            } => {
                *asked = (request.bytes_to_read() - *filled).min(MAX_CHUNK);
                HostRequest::Read {
                    handle: *handle,
                    len: if *read_ahead { MAX_CHUNK } else { *asked },
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
            PendingCall::PostWrite { .. } => {
                unreachable!("a posted write that waits for credit is not in the queue")
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
        event: HostEvent,
        core: &mut Core<'_>,
    ) -> anyhow::Result<HaltAction<R>> {
        let core_id = core.id();
        let reply = match event {
            HostEvent::Reply(reply) => reply,
            HostEvent::Credit(_) => return self.continue_post_write(core),
        };

        self.in_flight = None;
        let Some(call) = self.pending.get_mut(&core_id) else {
            bail!("A reply came for core {core_id}, which has no pending call");
        };

        let mut finished = true;
        match (call, reply.result) {
            (PendingCall::Open { request, mode, .. }, Ok(HostValue::Opened { handle, kind })) => {
                let read_ahead = kind == FileKind::File && *mode == OpenMode::Read;
                if read_ahead {
                    self.read_buffers.insert(handle, ReadBuffer::default());
                }
                let target_handle = self.handles.add(TargetHandle::Client {
                    handle,
                    kind,
                    read_ahead,
                });
                request.respond_with_handle(core, target_handle)?;
            }
            (
                PendingCall::Close {
                    request,
                    target,
                    handle,
                },
                result,
            ) => {
                let (request, target, handle) = (*request, *target, *handle);
                self.read_buffers.remove(&handle);
                // A write failed after the target saw success, and this is the last call that can
                // report it.
                if let Some(error) = self.posted.failed.remove(&handle) {
                    self.errno = error.errno();
                    self.handles.remove_client(target, handle);
                    self.forget(core_id);
                    return Ok(HaltAction::Resume);
                }
                self.handles.remove_client(target, handle);
                match result {
                    Ok(HostValue::Done) => request.success(core)?,
                    Ok(_) => bail!("The client sent a wrong reply"),
                    Err(error) => self.errno = error.errno(),
                }
            }
            (PendingCall::Seek { request, handle }, Ok(HostValue::Done)) => {
                if let Some(buffer) = self.read_buffers.get_mut(handle) {
                    buffer.reset(request.position());
                }
                request.success(core)?
            }
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
                    handle,
                    kind,
                    read_ahead,
                    filled,
                    asked,
                },
                Ok(HostValue::Read(bytes)),
            ) => {
                let read = bytes.len() as u32;
                let requested = if *read_ahead { MAX_CHUNK } else { *asked };
                if read > requested {
                    bail!("The client sent more data than the target asked for");
                }

                let for_target = read.min(*asked);
                request.write_chunk_to_target(core, *filled, &bytes[..for_target as usize])?;
                *filled += for_target;
                if let Some(buffer) = self.read_buffers.get_mut(handle) {
                    buffer.position += u64::from(for_target);
                    buffer.data.extend(&bytes[for_target as usize..]);
                }

                // A stream returns what it has. Another read could block for a long time.
                let more = read == requested
                    && *filled < request.bytes_to_read()
                    && *kind == FileKind::File;
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

    /// Credit came for the posted write of `core`.
    fn continue_post_write<R>(&mut self, core: &mut Core<'_>) -> anyhow::Result<HaltAction<R>> {
        let core_id = core.id();
        if !matches!(
            self.pending.get(&core_id),
            Some(PendingCall::PostWrite { .. })
        ) {
            // The run loop checked that the core still waits at the call.
            bail!("Credit came for core {core_id}, which has no waiting write");
        }
        let Some(PendingCall::PostWrite {
            request,
            handle,
            written,
        }) = self.pending.remove(&core_id)
        else {
            unreachable!("the call is a waiting write");
        };

        Ok(match self.post_write(request, handle, written, core)? {
            Some(call) => {
                self.wait_for(core_id, call);
                HaltAction::Wait
            }
            None => HaltAction::Resume,
        })
    }

    /// Waits for the posted writes, so that a failure reaches the user.
    pub fn drain(&mut self, token: &CancellationToken) -> anyhow::Result<()> {
        // Without file mappings, nothing was posted.
        let Some(replies) = &self.replies else {
            return Ok(());
        };
        while !self.posted.is_empty() && !token.is_cancelled() {
            if let Ok(reply) = replies.receiver().recv_timeout(Duration::from_millis(100)) {
                self.posted.complete(&reply);
            }
        }

        if !self.posted.is_empty() {
            tracing::warn!(
                "{} semihosting writes were not confirmed before the end.",
                self.posted.calls.len()
            );
        }
        if let Some(error) = self.posted.failed.values().next() {
            bail!("A semihosting write failed after the target continued: {error:?}");
        }
        Ok(())
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
        let call = self.pending.remove(&core_id);
        self.queue.retain(|core| *core != core_id);
        self.credit_waiters.retain(|core| *core != core_id);
        if self.in_flight.is_some_and(|(_, core)| core == core_id) {
            self.in_flight = None;
            // The client read for a lost reply, so the buffer no longer knows the position of the
            // client. Later reads and seeks of the handle go to the client.
            if let Some(PendingCall::Read {
                handle,
                read_ahead: true,
                ..
            }) = call
            {
                self.read_buffers.remove(&handle);
                self.handles.stop_read_ahead(handle);
            }
        }
    }

    /// Waits for an event for at most `timeout`. A reply to a posted write only returns if a core
    /// waits for its credit.
    pub fn wait(&mut self, timeout: Duration) -> Option<HostEvent> {
        let Some(replies) = &self.replies else {
            std::thread::sleep(timeout);
            return None;
        };
        // Credit that a core did not use, because it left its call, goes to the next one.
        if self.posted.has_credit()
            && let Some(core) = self.credit_waiters.pop_front()
        {
            return Some(HostEvent::Credit(core));
        }
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let reply = replies.receiver().recv_timeout(remaining).ok()?;
            if !self.posted.complete(&reply) {
                return Some(HostEvent::Reply(reply));
            }
            if self.posted.has_credit()
                && let Some(core) = self.credit_waiters.pop_front()
            {
                return Some(HostEvent::Credit(core));
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
        }
        | PendingCall::PostWrite {
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
    type Reply = HostEvent;

    fn on_halt(
        &mut self,
        reason: HaltReason,
        core: &mut Core<'_>,
    ) -> anyhow::Result<HaltAction<R>> {
        (self.on_halt)(reason, core, &mut self.host)
    }

    fn reply_core(&self, event: &HostEvent) -> Option<usize> {
        self.host.reply_core(event)
    }

    fn on_reply(&mut self, event: HostEvent, core: &mut Core<'_>) -> anyhow::Result<HaltAction<R>> {
        self.host.on_reply(event, core)
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

    fn drain(&mut self, token: &CancellationToken) -> anyhow::Result<()> {
        self.host.drain(token)
    }

    fn wait(&mut self, timeout: Duration) -> Option<HostEvent> {
        self.host.wait(timeout)
    }
}

#[cfg(test)]
mod test {
    use super::*;

    const CLIENT: TargetHandle = TargetHandle::Client {
        handle: 7,
        kind: FileKind::File,
        read_ahead: false,
    };

    fn written(id: u64, count: u32) -> HostReply {
        HostReply {
            id,
            result: Ok(HostValue::Written(count)),
        }
    }

    #[test]
    fn posted_writes_are_limited_by_count_and_bytes() {
        let mut posted = PostedWrites::default();
        for id in 0..MAX_POSTED_WRITES as u64 {
            assert!(posted.has_credit());
            posted.add(id, 1, 1);
        }
        assert!(!posted.has_credit());
        assert!(posted.complete(&written(0, 1)));
        assert!(posted.has_credit());

        let mut posted = PostedWrites::default();
        posted.add(0, 1, MAX_POSTED_BYTES - MAX_CHUNK + 1);
        assert!(!posted.has_credit());
    }

    #[test]
    fn a_failed_posted_write_latches_on_its_handle() {
        let mut posted = PostedWrites::default();
        posted.add(0, 1, 10);
        posted.add(1, 1, 10);
        posted.add(2, 2, 10);

        assert!(posted.complete(&HostReply {
            id: 0,
            result: Err(HostError::NotFound),
        }));
        // A short write loses data too. The first failure stays.
        assert!(posted.complete(&written(1, 5)));
        assert!(posted.complete(&written(2, 10)));

        assert_eq!(posted.failed.get(&1), Some(&HostError::NotFound));
        assert_eq!(posted.failed.get(&2), None);
        assert!(posted.is_empty());
    }

    #[test]
    fn a_reply_to_another_call_is_not_a_posted_write() {
        let mut posted = PostedWrites::default();
        posted.add(0, 1, 10);

        assert!(!posted.complete(&written(1, 10)));
        assert!(!posted.is_empty());
    }

    #[test]
    fn a_read_buffer_serves_reads_and_forward_seeks() {
        let mut buffer = ReadBuffer::default();
        buffer.data.extend(b"0123456789");

        assert_eq!(buffer.take(2), b"01");
        assert!(buffer.seek(5));
        assert_eq!(buffer.take(2), b"56");
        assert_eq!(buffer.take(100), b"789");
        assert!(buffer.take(1).is_empty());

        // Earlier data is gone, and later data needs the client.
        assert!(!buffer.seek(3));
        assert!(!buffer.seek(11));
        assert!(buffer.seek(10));
    }

    #[test]
    fn a_seek_on_the_client_replaces_the_buffer() {
        let mut buffer = ReadBuffer::default();
        buffer.data.extend(b"0123");

        buffer.reset(100);

        assert!(buffer.take(1).is_empty());
        assert!(buffer.seek(100));
    }

    #[test]
    fn the_buffer_reads_past_4_gib() {
        let mut buffer = ReadBuffer::default();
        buffer.reset(u32::MAX - 1);
        buffer.data.extend(b"0123");

        assert_eq!(buffer.take(4), b"0123");

        // A seek of the target cannot reach this far, so it goes to the client.
        assert!(!buffer.seek(u32::MAX));
    }

    #[test]
    fn a_handle_can_stop_its_read_ahead() {
        let mut table = HandleTable::default();
        let handle = table.add(TargetHandle::Client {
            handle: 7,
            kind: FileKind::File,
            read_ahead: true,
        });

        table.stop_read_ahead(7);

        assert!(matches!(
            table.get(handle.get()),
            Some(TargetHandle::Client {
                read_ahead: false,
                ..
            })
        ));
    }

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
            read_ahead: false,
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
