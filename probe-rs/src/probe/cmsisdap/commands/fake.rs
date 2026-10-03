//! A scripted CMSIS-DAP device for host tests.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::{CommandId, DEFAULT_USB_TIMEOUT};

/// How the probe answers one command.
pub(crate) enum Reply {
    /// The reply is ready at once, unless an earlier reply is still held.
    Now(Vec<u8>),
    /// The probe is slow: the reply, and every reply after it, waits until it is released.
    Held(Vec<u8>),
    /// The command has no reply.
    None,
}

type Responder = Box<dyn FnMut(&[u8]) -> Reply + Send>;

/// The state of a fake probe, shared between the device and the test.
pub(crate) struct FakeProbe {
    /// Every command written, from the command ID on.
    pub(crate) sent: Vec<Vec<u8>>,
    /// Replies that a read or a drain can take.
    ready: VecDeque<Vec<u8>>,
    /// Replies that arrive while a read waits, after any drain before it.
    late: VecDeque<Vec<u8>>,
    /// Replies that the probe has not finished yet.
    held: VecDeque<Vec<u8>>,
    /// Whether a read timed out while replies were held.
    timed_out: bool,
    /// Whether `DAP_TransferAbort` ends a held transfer at once. Otherwise the abort waits
    /// behind it, as on debugprobe.
    pub(crate) abort_interrupts: bool,
    pub(crate) max_packet_size: usize,
    pub(crate) usb_timeout: Duration,
    respond: Responder,
}

impl FakeProbe {
    /// A probe that answers with `respond`, shared with the caller.
    pub(crate) fn shared(
        max_packet_size: usize,
        respond: impl FnMut(&[u8]) -> Reply + Send + 'static,
    ) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            sent: Vec::new(),
            ready: VecDeque::new(),
            late: VecDeque::new(),
            held: VecDeque::new(),
            timed_out: false,
            abort_interrupts: true,
            max_packet_size,
            usb_timeout: DEFAULT_USB_TIMEOUT,
            respond: Box::new(respond),
        }))
    }

    pub(crate) fn write(&mut self, command: &[u8]) {
        self.sent.push(command.to_vec());
        let abort = command.first() == Some(&(CommandId::TransferAbort as u8));
        if abort && self.abort_interrupts {
            self.ready.extend(self.late.drain(..));
            self.ready.extend(self.held.drain(..));
            self.timed_out = false;
        } else if !abort && std::mem::take(&mut self.timed_out) {
            // The probe finished while the host gave up on the read.
            self.late.extend(self.held.drain(..));
        }
        match (self.respond)(command) {
            Reply::Held(reply) => self.held.push_back(reply),
            Reply::Now(reply) if !self.held.is_empty() => self.held.push_back(reply),
            Reply::Now(reply) if !self.late.is_empty() => self.late.push_back(reply),
            Reply::Now(reply) => self.ready.push_back(reply),
            Reply::None => {}
        }
    }

    /// Returns the next reply, or `None` for a read that times out.
    pub(crate) fn read(&mut self) -> Option<Vec<u8>> {
        let reply = self.ready.pop_front().or_else(|| self.late.pop_front());
        if reply.is_none() {
            self.timed_out = true;
        }
        reply
    }

    /// How many `DAP_TransferAbort` commands were written.
    pub(crate) fn aborts(&self) -> usize {
        let abort = CommandId::TransferAbort as u8;
        self.sent
            .iter()
            .filter(|command| command[0] == abort)
            .count()
    }

    /// Whether the probe owes no reply.
    pub(crate) fn owes_nothing(&self) -> bool {
        self.ready.is_empty() && self.late.is_empty() && self.held.is_empty()
    }

    /// Discards the ready replies, and returns whether there were any. The late ones arrive
    /// after it.
    pub(crate) fn drain(&mut self) -> bool {
        let drained = !self.ready.is_empty();
        self.ready.clear();
        self.ready.extend(self.late.drain(..));
        drained
    }
}

/// The reply of a probe with `packet_size` byte packets and `packet_count` buffers to the
/// commands that opening a probe sends. Any other command is unknown.
pub(crate) fn standard_reply(command: &[u8], packet_size: u16, packet_count: u8) -> Reply {
    const SWD: u8 = 0x01;
    let [lo, hi] = packet_size.to_le_bytes();
    match command {
        [0x00, 0xFF, ..] => Reply::Now(vec![0x00, 0x02, lo, hi]),
        [0x00, 0xFE, ..] => Reply::Now(vec![0x00, 0x01, packet_count]),
        [0x00, 0xF0, ..] => Reply::Now(vec![0x00, 0x01, SWD]),
        [0x01, ..] => Reply::Now(vec![0x01, 0x00]),
        [0x07, ..] => Reply::None,
        _ => Reply::Now(vec![0xFF]),
    }
}
