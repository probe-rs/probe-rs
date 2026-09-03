//! The SWD engine: register accesses and line sequences batched into `0xE8` commands.
//!
//! The firmware clocks whole transactions and samples the ACK; the host encodes requests,
//! checks parity, and keeps every batch inside the chip's buffers and processing window.
//!
//! A batch runs to its end with the debug port's overrun detection off, since this
//! firmware mishandles the data phase it requires, so an access after a WAIT still ran.
//! That is why the driver owns WAIT itself rather than letting the host replay from the
//! WAITed access: reads keep their values, as the DP answers posted reads in order, and
//! the WAITed ones are retried with a short backoff; a WAIT with a write after it is
//! reported as is, so nothing is applied twice. A core left asleep outlasts the retries
//! and errors. Every read gets its own value, so the host adds no RDBUFF reads.

use std::thread::sleep;
use std::time::Duration;

use crate::architecture::arm::dp::{Abort, Ctrl};
use crate::probe::{
    BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbeError, Direction,
    HandleId, Port, Results, SwdBatch, SwdOp, SwdTransferError,
};

use super::device::Ch347Device;
use super::transport::{Ch347Error, HEADER_LEN, MAX_PACKET};

const CMD_SWD: u8 = 0xE8;

const SUB_WRITE: u8 = 0xA0;
const SUB_SEQUENCE: u8 = 0xA1;
const SUB_READ: u8 = 0xA2;
/// Wire bits of a write: request, ACK, data and parity.
const WRITE_BITS: u8 = 41;
/// Wire bits of a read: request, ACK, data and parity, minus the second turnaround.
const READ_BITS: u8 = 34;
/// SWCLK cycles the firmware spends per register access.
const ACCESS_CLOCKS: u32 = 46;
/// The firmware runs a whole batch before answering, and the USB host drops the device past
/// about 8 ms of silence.
const MAX_BATCH_NS: u64 = 7_000_000;
/// Idle clocks appended to every batch of accesses so the last one completes in the target.
const TRAILING_IDLE: u8 = 8;
/// The most bits one sequence sub-command carries.
const MAX_SEQUENCE_BITS: usize = 64;

const ACK_OK: u8 = 0b001;
const ACK_WAIT: u8 = 0b010;
const ACK_FAULT: u8 = 0b100;
const ACK_NONE: u8 = 0b111;

/// Retries after a WAIT; the pause before each doubles from [`WAIT_BACKOFF`] up to 64
/// times it, about 110 ms in all, which covers a core sleeping between timer ticks.
pub(super) const WAIT_RETRIES: u32 = 12;
pub(super) const WAIT_BACKOFF: Duration = Duration::from_micros(250);

/// SWCLK = 1 MHz / divisor, or 5 MHz for divisor 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SwdClock(u8);

impl SwdClock {
    pub const BASE_HZ: u32 = 1_000_000;
    const BASE_KHZ: u32 = Self::BASE_HZ / 1000;
    const FAST_KHZ: u32 = 5000;
    /// OpenOCD's floor, about 32 kHz; nothing this driver sends needs a slower clock.
    const MAX_DIVISOR: u32 = 31;

    /// The fastest setting at or below `khz`.
    pub fn new(khz: u32, five_mhz: bool) -> Result<Self, DebugProbeError> {
        if khz >= Self::FAST_KHZ && five_mhz {
            return Ok(Self(0));
        }
        let divisor = Self::BASE_KHZ.div_ceil(khz.max(1));
        if divisor > Self::MAX_DIVISOR {
            return Err(DebugProbeError::UnsupportedSpeed(khz));
        }
        Ok(Self(divisor as u8))
    }

    pub fn fastest(five_mhz: bool) -> Self {
        Self(if five_mhz { 0 } else { 1 })
    }

    pub fn divisor(self) -> u8 {
        self.0
    }

    pub fn khz(self) -> u32 {
        match self.0 {
            0 => Self::FAST_KHZ,
            divisor => Self::BASE_KHZ / u32::from(divisor),
        }
    }

    fn period_ns(self) -> u64 {
        match self.0 {
            0 => 200,
            divisor => u64::from(divisor) * 1000,
        }
    }
}

/// A DP or AP register: the port and address bits 2 and 3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Register {
    port: Port,
    addr: u8,
}

impl Register {
    const ABORT: Self = Self::dp(0x0);
    const CTRL_STAT: Self = Self::dp(0x4);
    /// TARGETSEL as a write, RDBUFF as a read.
    const DP_C: Self = Self::dp(0xC);

    const fn dp(addr: u8) -> Self {
        Self {
            port: Port::Dp,
            addr,
        }
    }

    fn new(port: Port, addr: u8) -> Self {
        Self {
            port,
            addr: addr & 0xC,
        }
    }

    fn is_ap(self) -> bool {
        self.port == Port::Ap
    }

    /// The SWD request byte: start, APnDP, RnW, A[2:3], parity, stop, park.
    fn request(self, read: bool) -> u8 {
        let ap = self.is_ap();
        let a2 = self.addr & 0x4 != 0;
        let a3 = self.addr & 0x8 != 0;
        let parity = ap ^ read ^ a2 ^ a3;
        let bit = |set: bool, position: u8| u8::from(set) << position;
        0x81 | bit(ap, 1) | bit(read, 2) | bit(a2, 3) | bit(a3, 4) | bit(parity, 5)
    }
}

fn parity(value: u32) -> u8 {
    (value.count_ones() & 1) as u8
}

#[derive(Debug, Clone, Copy)]
enum Op {
    Write {
        register: Register,
        value: u32,
        /// TARGETSEL writes get no ACK by design.
        check_ack: bool,
    },
    Read {
        register: Register,
    },
    Sequence {
        bits: u8,
        data: u64,
    },
}

impl Op {
    fn write(register: Register, value: u32) -> Self {
        Op::Write {
            register,
            value,
            check_ack: register != Register::DP_C,
        }
    }

    fn read(register: Register) -> Self {
        Op::Read { register }
    }

    fn sequence(bits: u8, data: u64) -> Self {
        Op::Sequence { bits, data }
    }

    fn idle(bits: u8) -> Self {
        Self::sequence(bits, 0)
    }

    fn sub(self) -> u8 {
        match self {
            Op::Write { .. } => SUB_WRITE,
            Op::Read { .. } => SUB_READ,
            Op::Sequence { .. } => SUB_SEQUENCE,
        }
    }

    fn sequence_bytes(bits: u8) -> usize {
        usize::from(bits).div_ceil(8)
    }

    fn send_len(self) -> usize {
        match self {
            Op::Write { .. } => 9,
            Op::Read { .. } => 4,
            Op::Sequence { bits, .. } => 3 + Self::sequence_bytes(bits),
        }
    }

    fn recv_len(self) -> usize {
        match self {
            Op::Write { .. } => 2,
            Op::Read { .. } => 7,
            Op::Sequence { .. } => 1,
        }
    }

    fn clocks(self) -> u32 {
        match self {
            Op::Write { .. } | Op::Read { .. } => ACCESS_CLOCKS,
            Op::Sequence { bits, .. } => u32::from(bits),
        }
    }

    /// Whether the batch may be retried after a WAIT earlier in it: a register write after
    /// the WAIT ran out of order, reads and sequences did no harm.
    fn harmless(self) -> bool {
        !matches!(self, Op::Write { .. })
    }

    /// The register a read answers for.
    fn reads(self) -> Option<Register> {
        match self {
            Op::Read { register } => Some(register),
            _ => None,
        }
    }

    fn encode(self, out: &mut Vec<u8>) {
        out.push(self.sub());
        match self {
            Op::Write {
                register, value, ..
            } => {
                out.extend_from_slice(&[WRITE_BITS, 0, register.request(false)]);
                out.extend_from_slice(&value.to_le_bytes());
                out.push(parity(value));
            }
            Op::Read { register } => out.extend_from_slice(&[READ_BITS, 0, register.request(true)]),
            Op::Sequence { bits, data } => {
                out.extend_from_slice(&[bits, 0]);
                out.extend_from_slice(&data.to_le_bytes()[..Self::sequence_bytes(bits)]);
            }
        }
    }
}

/// The operations of one command, kept inside the firmware's buffers and time window.
#[derive(Debug)]
struct Batch {
    ops: Vec<Op>,
    /// How many of `ops` are the tail: budgeted from the start and kept through retries.
    tail_len: usize,
    send: usize,
    recv: usize,
    clocks: u32,
    clock: SwdClock,
}

impl Batch {
    fn new(clock: SwdClock, tail: impl IntoIterator<Item = Op>) -> Self {
        let mut batch = Self {
            ops: Vec::new(),
            tail_len: 0,
            send: HEADER_LEN,
            recv: HEADER_LEN,
            clocks: 0,
            clock,
        };
        for op in tail {
            batch.insert(op);
        }
        batch.tail_len = batch.ops.len();
        batch
    }

    /// Register accesses end with idle clocks; sequences must stay adjacent to each other.
    fn accesses(clock: SwdClock, tail: Option<Op>) -> Self {
        Self::new(clock, tail.into_iter().chain([Op::idle(TRAILING_IDLE)]))
    }

    fn fits(&self, op: Op) -> bool {
        self.send + op.send_len() <= MAX_PACKET
            && self.recv + op.recv_len() <= MAX_PACKET
            && u64::from(self.clocks + op.clocks()) * self.clock.period_ns() <= MAX_BATCH_NS
    }

    /// Adds an op before the tail; the caller has checked that it fits.
    fn insert(&mut self, op: Op) {
        self.send += op.send_len();
        self.recv += op.recv_len();
        self.clocks += op.clocks();
        self.ops.insert(self.ops.len() - self.tail_len, op);
    }

    fn push(&mut self, op: Op) -> Result<(), DebugProbeError> {
        if !self.fits(op) {
            return Err(too_large());
        }
        self.insert(op);
        Ok(())
    }

    /// Adds ops while they fit and returns how many; the first must.
    fn fill(&mut self, ops: impl IntoIterator<Item = Op>) -> Result<usize, DebugProbeError> {
        let mut count = 0;
        for op in ops {
            if !self.fits(op) {
                break;
            }
            self.insert(op);
            count += 1;
        }
        if count == 0 {
            return Err(too_large());
        }
        Ok(count)
    }

    fn own_len(&self) -> usize {
        self.ops.len() - self.tail_len
    }

    fn payload(&self) -> Vec<u8> {
        let mut payload = Vec::with_capacity(self.send - HEADER_LEN);
        for op in &self.ops {
            op.encode(&mut payload);
        }
        payload
    }

    /// Keeps the ops at `positions` and the tail, for a retry.
    fn keep(&mut self, positions: &[usize]) {
        let tail_from = self.own_len();
        let kept: Vec<Op> = (0..self.ops.len())
            .filter(|position| *position >= tail_from || positions.contains(position))
            .map(|position| self.ops[position])
            .collect();
        let tail_len = self.tail_len;
        *self = Self::new(self.clock, kept);
        self.tail_len = tail_len;
    }
}

fn too_large() -> DebugProbeError {
    Ch347Error::BatchTooLarge.into()
}

/// One op's reply: a read's value, or the ACK error the access got.
type Reply = Result<Option<u32>, SwdTransferError>;

fn ack_error(ack: u8) -> Result<(), SwdTransferError> {
    match ack & 0b111 {
        ACK_OK => Ok(()),
        ACK_WAIT => Err(SwdTransferError::WaitResponse),
        ACK_FAULT => Err(SwdTransferError::FaultResponse),
        ACK_NONE => Err(SwdTransferError::NoAcknowledge),
        _ => Err(SwdTransferError::Protocol),
    }
}

/// Decodes the sub-replies in operation order; `None` means the reply does not match.
fn parse(ops: &[Op], reply: &[u8]) -> Option<Vec<Reply>> {
    let mut replies = Vec::with_capacity(ops.len());
    let mut pos = 0;
    for op in ops {
        if reply.get(pos) != Some(&op.sub()) {
            return None;
        }
        pos += 1;
        replies.push(match *op {
            Op::Sequence { .. } => Ok(None),
            Op::Write { check_ack, .. } => {
                let ack = *reply.get(pos)?;
                pos += 1;
                if check_ack {
                    ack_error(ack).map(|()| None)
                } else {
                    Ok(None)
                }
            }
            Op::Read { .. } => {
                let bytes = reply.get(pos..pos + 6)?;
                pos += 6;
                ack_error(bytes[0]).and_then(|()| {
                    let value = u32::from_le_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
                    if bytes[5] & 1 != parity(value) {
                        return Err(SwdTransferError::IncorrectParity);
                    }
                    Ok(Some(value))
                })
            }
        });
    }
    (pos == reply.len()).then_some(replies)
}

/// What a batch's own ops produced: each read's value by position, and where the batch
/// stopped if an access failed.
struct Run {
    values: Vec<Option<u32>>,
    failed: Option<(usize, SwdTransferError)>,
}

/// One transfer of a host batch, with its position in that batch.
struct Transfer {
    index: usize,
    id: HandleId,
    op: Op,
}

impl Ch347Device {
    fn batch(&self, tail: Option<Op>) -> Batch {
        Batch::accesses(self.swd_clock(), tail)
    }

    fn execute(&mut self, batch: &Batch) -> Result<Vec<Reply>, DebugProbeError> {
        let reply = self.command(CMD_SWD, &batch.payload(), batch.recv - HEADER_LEN)?;
        match parse(&batch.ops, &reply) {
            Some(replies) => Ok(replies),
            None => Err(self.desynced(CMD_SWD, reply)),
        }
    }

    /// Runs a batch, retrying WAITed accesses while that is safe.
    ///
    /// An AP read answers with the value of the AP read before it, so a value is credited
    /// to the earliest read of that register still without one, which is also the order
    /// the words of an auto-incrementing block arrive in.
    fn run(&mut self, mut batch: Batch) -> Result<Run, DebugProbeError> {
        let readers: Vec<Option<Register>> = batch.ops[..batch.own_len()]
            .iter()
            .map(|op| op.reads())
            .collect();
        let mut values = vec![None; readers.len()];
        let mut posted = None;
        // The batch position of every own op still to run, in order.
        let mut outstanding: Vec<usize> = (0..readers.len()).collect();
        let mut retries = 0;

        let earliest = |values: &[Option<u32>], register| {
            (0..readers.len())
                .find(|&index| readers[index] == Some(register) && values[index].is_none())
        };
        let credit = |values: &mut Vec<Option<u32>>, register, value| {
            if let Some(index) = earliest(values, register) {
                values[index] = Some(value);
            }
        };
        // Where a failure of a tail op is reported: the read it was fetching for.
        let failed_at = |values: &[Option<u32>], index: Option<usize>, posted: Option<Register>| {
            index
                .or_else(|| posted.and_then(|register| earliest(values, register)))
                .unwrap_or(readers.len().saturating_sub(1))
        };

        loop {
            let replies = self.execute(&batch)?;
            let mut waited = Vec::new();
            for (position, (&op, reply)) in batch.ops.iter().zip(replies).enumerate() {
                let index = outstanding.get(position).copied();
                match (op, reply) {
                    (Op::Read { register }, Ok(Some(value))) => {
                        if register == Register::DP_C {
                            if let Some(register) = posted.take() {
                                credit(&mut values, register, value);
                            }
                        } else if register.is_ap() {
                            if let Some(register) = posted.replace(register) {
                                credit(&mut values, register, value);
                            }
                        } else if let Some(index) = index {
                            values[index] = Some(value);
                        }
                    }
                    (_, Ok(_)) => {}
                    (_, Err(SwdTransferError::WaitResponse)) => waited.push(position),
                    (_, Err(error)) => {
                        if error == SwdTransferError::FaultResponse {
                            self.clear_fault();
                        }
                        let at = failed_at(&values, index, posted);
                        return Ok(Run {
                            values,
                            failed: Some((at, error)),
                        });
                    }
                }
            }
            let Some(&first) = waited.first() else {
                return Ok(Run {
                    values,
                    failed: None,
                });
            };
            if retries == WAIT_RETRIES || !batch.ops[first + 1..].iter().all(|op| op.harmless()) {
                self.abort_wait();
                let at = failed_at(&values, outstanding.get(first).copied(), posted);
                return Ok(Run {
                    values,
                    failed: Some((at, SwdTransferError::WaitResponse)),
                });
            }
            tracing::debug!("WAIT at access {first}, retry {}", retries + 1);
            sleep(self.wait_backoff * (1 << retries.min(6)));
            retries += 1;
            outstanding = waited
                .iter()
                .filter_map(|&position| outstanding.get(position).copied())
                .collect();
            batch.keep(&waited);
        }
    }

    /// Frees the AP from the transaction that kept it busy, best effort.
    fn abort_wait(&mut self) {
        let mut abort = Abort(0);
        abort.set_dapabort(true);
        if let Err(error) = self.write_abort(abort) {
            tracing::debug!("DAPABORT after WAIT failed: {error}");
        }
    }

    /// Reads CTRL/STAT for the log and clears the sticky flags, best effort.
    fn clear_fault(&mut self) {
        let mut batch = self.batch(None);
        if batch.push(Op::read(Register::CTRL_STAT)).is_ok()
            && let Ok(replies) = self.execute(&batch)
            && let Some(Ok(Some(value))) = replies.first()
        {
            tracing::debug!("CTRL/STAT after FAULT: {:?}", Ctrl::try_from(*value));
        }
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        if let Err(error) = self.write_abort(abort) {
            tracing::warn!("clearing sticky errors after FAULT failed: {error}");
        }
    }

    fn write_abort(&mut self, abort: Abort) -> Result<(), DebugProbeError> {
        let mut batch = self.batch(None);
        batch.push(Op::write(Register::ABORT, abort.into()))?;
        self.execute(&batch).map(drop)
    }

    /// Runs a host batch: transfers go out in as few commands as fit, sequences and idle
    /// clocks each as their own.
    pub(crate) fn run_swd_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();
        let mut transfers = Vec::new();
        for (index, (id, op)) in batch.iter().enumerate() {
            let line = match *op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
                    let register = Register::new(port, addr);
                    let op = match direction {
                        Direction::Read => Op::read(register),
                        Direction::Write => Op::write(register, data),
                    };
                    transfers.push(Transfer {
                        index,
                        id: id.clone(),
                        op,
                    });
                    continue;
                }
                SwdOp::Sequence(ref bits) => self.line_sequence(bits),
                SwdOp::Idle { cycles } => {
                    self.line_sequence(&BitSequence::repeat(false, cycles as usize))
                }
                SwdOp::Pins { .. } => Err(DebugProbeError::CommandNotSupportedByProbe {
                    command_name: "swj_pins",
                }),
            };
            self.run_transfers(&mut transfers, &mut results)?;
            if let Err(error) = line {
                return Err(BatchExecutionError::new_from_debug_probe_at(
                    error, results, index,
                ));
            }
        }
        self.run_transfers(&mut transfers, &mut results)?;
        Ok(results)
    }

    /// Runs the pending transfers in command-sized chunks and records their reads.
    fn run_transfers(
        &mut self,
        transfers: &mut Vec<Transfer>,
        results: &mut Results,
    ) -> Result<(), BatchExecutionError<DebugProbeError>> {
        while !transfers.is_empty() {
            let posted_reads = transfers
                .iter()
                .any(|transfer| transfer.op.reads().is_some_and(Register::is_ap));
            let mut batch = self.batch(posted_reads.then_some(Op::read(Register::DP_C)));
            let first = transfers[0].index;
            let probe_error = |error, results: &mut Results| {
                BatchExecutionError::new_from_debug_probe_at(error, std::mem::take(results), first)
            };
            let count = batch
                .fill(transfers.iter().map(|transfer| transfer.op))
                .map_err(|error| probe_error(error, results))?;
            let chunk: Vec<Transfer> = transfers.drain(..count).collect();
            let run = self
                .run(batch)
                .map_err(|error| probe_error(error, results))?;

            let stop = run.failed.map_or(usize::MAX, |(at, _)| at);
            for (transfer, value) in chunk.iter().zip(run.values) {
                if let Some(value) = value
                    && transfer.index < stop
                    && transfer.id.should_capture()
                {
                    results.push(&transfer.id, CommandResult::U32(value));
                }
            }
            if let Some((at, error)) = run.failed {
                return Err(BatchExecutionError {
                    error: BatchError::Specific(DebugProbeError::SwdTransfer(error)),
                    results: std::mem::take(results),
                    fault_operation: chunk[at.min(chunk.len() - 1)].index,
                });
            }
        }
        Ok(())
    }

    /// Clocks `bits` out on SWDIO, LSB first, as its own commands.
    fn line_sequence(&mut self, bits: &BitSequence) -> Result<(), DebugProbeError> {
        let mut start = 0;
        while start < bits.len() {
            let len = (bits.len() - start).min(MAX_SEQUENCE_BITS);
            let mut data = 0;
            for (position, bit) in bits.slice(start, len).iter().enumerate() {
                data |= u64::from(bit) << position;
            }
            let op = Op::sequence(len as u8, data);
            let mut payload = Vec::with_capacity(op.send_len());
            op.encode(&mut payload);
            let reply = self.command(CMD_SWD, &payload, 1)?;
            if reply != [SUB_SEQUENCE] {
                return Err(self.desynced(CMD_SWD, reply));
            }
            start += len;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::WireProtocol;
    use crate::probe::ch347::device::tests::{CH347F_1_20, device, scripted};

    const DPIDR: Register = Register::dp(0x0);
    const CTRL: Register = Register::CTRL_STAT;
    const SELECT: Register = Register::dp(0x8);
    const ABORT: Register = Register::ABORT;
    const RDBUFF: Register = Register::DP_C;
    const TARGETSEL: Register = Register::DP_C;
    const CSW: Register = Register {
        port: Port::Ap,
        addr: 0x0,
    };
    const DRW: Register = Register {
        port: Port::Ap,
        addr: 0xC,
    };

    /// The ABORT write with DAPABORT that follows a WAIT the driver gives up on.
    const DAPABORT: &[u8] = &[0xA0, 0x29, 0, 0x81, 1, 0, 0, 0, 1, 0xA1, 8, 0, 0];
    const WRITE_OK: &[u8] = &[SUB_WRITE, ACK_OK, SUB_SEQUENCE];

    type Failure = BatchExecutionError<DebugProbeError>;

    fn frame(payload: &[u8]) -> Vec<u8> {
        crate::probe::ch347::transport::frame(CMD_SWD, payload)
    }

    // The request/reply builders below compose frames from named operations so a test reads
    // as the exchange it checks. The byte encoding they lean on is pinned independently by
    // `operations_encode_like_openocd` and `request_bytes_match_the_specification`.

    /// A request frame: the operations encoded, then the trailing idle the driver appends.
    fn req(ops: &[Op]) -> Vec<u8> {
        let mut payload = vec![];
        for &op in ops {
            op.encode(&mut payload);
        }
        Op::idle(TRAILING_IDLE).encode(&mut payload);
        frame(&payload)
    }

    /// A reply frame: the sub-replies, then the trailing idle's own answer.
    fn reply(subs: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes: Vec<u8> = subs.concat();
        bytes.push(SUB_SEQUENCE);
        frame(&bytes)
    }

    fn read_reply(ack: u8, value: u32) -> Vec<u8> {
        let mut reply = vec![SUB_READ, ack];
        reply.extend_from_slice(&value.to_le_bytes());
        reply.push(parity(value) | 0b10);
        reply
    }

    /// A successful read's sub-reply carrying `value`.
    fn ok(value: u32) -> Vec<u8> {
        read_reply(ACK_OK, value)
    }

    /// A read's sub-reply that WAITed.
    fn wait() -> Vec<u8> {
        read_reply(ACK_WAIT, 0)
    }

    /// A write's sub-reply with the given ACK.
    fn wrote(ack: u8) -> Vec<u8> {
        vec![SUB_WRITE, ack]
    }

    // The helpers below drive the engine the way the host does, through a batch.

    fn read_block(
        dev: &mut Ch347Device,
        register: Register,
        count: usize,
    ) -> Result<Vec<u32>, Failure> {
        let mut batch = SwdBatch::new();
        let handles: Vec<_> = (0..count)
            .map(|_| batch.read(register.port, register.addr))
            .collect();
        let mut results = dev.run_swd_batch(&batch)?;
        Ok(handles
            .into_iter()
            .map(|handle| {
                results
                    .take(handle)
                    .unwrap_or_else(|_| panic!("the read has a value"))
            })
            .collect())
    }

    fn read(dev: &mut Ch347Device, register: Register) -> Result<u32, Failure> {
        Ok(read_block(dev, register, 1)?[0])
    }

    fn write_block(
        dev: &mut Ch347Device,
        register: Register,
        values: &[u32],
    ) -> Result<(), Failure> {
        let mut batch = SwdBatch::new();
        for &value in values {
            batch.write(register.port, register.addr, value);
        }
        dev.run_swd_batch(&batch).map(drop)
    }

    fn write(dev: &mut Ch347Device, register: Register, value: u32) -> Result<(), Failure> {
        write_block(dev, register, &[value])
    }

    /// The ACK error a failed batch reports, with the operation it stopped at.
    fn transfer_error(failure: Failure) -> (usize, SwdTransferError) {
        match failure.error {
            BatchError::Specific(DebugProbeError::SwdTransfer(error)) => {
                (failure.fault_operation, error)
            }
            error => panic!("not a transfer error: {error:?}"),
        }
    }

    #[test]
    fn request_bytes_match_the_specification() {
        assert_eq!(DPIDR.request(true), 0xA5);
        assert_eq!(RDBUFF.request(true), 0xBD);
        assert_eq!(ABORT.request(false), 0x81);
        assert_eq!(SELECT.request(false), 0xB1);
        assert_eq!(CSW.request(false), 0xA3);
        assert_eq!(DRW.request(true), 0x9F);
    }

    #[test]
    // This work is based on the OpenOCD impl so lets make sure we do the same things.
    fn operations_encode_like_openocd() {
        let mut out = vec![];
        Op::write(ABORT, 0x1E).encode(&mut out);
        assert_eq!(out, [0xA0, 0x29, 0, 0x81, 0x1E, 0, 0, 0, 0]);
        out.clear();
        Op::write(DRW, 0x8000_0001).encode(&mut out);
        assert_eq!(out, [0xA0, 0x29, 0, 0xBB, 1, 0, 0, 0x80, 0]);
        out.clear();
        Op::read(DPIDR).encode(&mut out);
        assert_eq!(out, [0xA2, 0x22, 0, 0xA5]);
        out.clear();
        Op::sequence(16, 0xE79E).encode(&mut out);
        assert_eq!(out, [0xA1, 16, 0, 0x9E, 0xE7]);
        out.clear();
        Op::sequence(51, u64::MAX).encode(&mut out);
        assert_eq!(out, [0xA1, 51, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn targetsel_writes_ignore_the_ack() {
        assert!(matches!(
            Op::write(TARGETSEL, 0),
            Op::Write {
                check_ack: false,
                ..
            }
        ));
        assert!(matches!(
            Op::write(DPIDR, 0),
            Op::Write {
                check_ack: true,
                ..
            }
        ));
    }

    #[test]
    fn swd_clock_rounds_down() {
        assert_eq!(SwdClock::new(8000, true).unwrap(), SwdClock(0));
        assert_eq!(SwdClock::new(8000, false).unwrap(), SwdClock(1));
        assert_eq!(SwdClock::new(4000, true).unwrap(), SwdClock(1));
        assert_eq!(SwdClock::new(999, true).unwrap(), SwdClock(2));
        assert_eq!(SwdClock::new(300, true).unwrap().khz(), 250);
        assert_eq!(SwdClock::new(33, true).unwrap(), SwdClock(31));
        assert!(SwdClock::new(32, true).is_err());
        assert!(SwdClock::new(0, true).is_err());
    }

    #[test]
    fn dp_read_returns_the_value() {
        let (mut dev, script) = device(&[(&req(&[Op::read(DPIDR)]), &reply(&[ok(0x2BA0_1477)]))]);
        assert_eq!(read(&mut dev, DPIDR).unwrap(), 0x2BA0_1477);
        assert!(script.finished());
    }

    #[test]
    fn ap_read_discards_the_posted_value() {
        // An AP read posts its result; the driver fetches it with a following RDBUFF read
        // and returns that, discarding the stale value the posted read answered with.
        let (mut dev, script) = device(&[(
            &req(&[Op::read(DRW), Op::read(RDBUFF)]),
            &reply(&[ok(0xDEAD_BEEF), ok(0x1234_5678)]),
        )]);
        assert_eq!(read(&mut dev, DRW).unwrap(), 0x1234_5678);
        assert!(script.finished());
    }

    #[test]
    fn mixed_dp_and_ap_reads_each_get_their_own_value() {
        // A DP read in between leaves the posted AP value in place, so the second AP read
        // still answers for the first and RDBUFF for the second.
        let (mut dev, script) = device(&[(
            &req(&[
                Op::read(DRW),
                Op::read(CTRL),
                Op::read(DRW),
                Op::read(RDBUFF),
            ]),
            &reply(&[ok(0xDEAD_BEEF), ok(0xC7), ok(1), ok(2)]),
        )]);
        let mut batch = SwdBatch::new();
        let first = batch.read(Port::Ap, 0xC);
        let ctrl = batch.read(Port::Dp, 0x4);
        let second = batch.read(Port::Ap, 0xC);
        let mut results = dev.run_swd_batch(&batch).unwrap();
        assert_eq!(results.take(first).ok(), Some(1));
        assert_eq!(results.take(ctrl).ok(), Some(0xC7));
        assert_eq!(results.take(second).ok(), Some(2));
        assert!(script.finished());
    }

    #[test]
    fn a_write_wait_is_retried_with_backoff_then_gives_up() {
        let request = req(&[Op::write(CSW, 0x12)]);
        // The idle after the write is side-effect free, so the write itself is retried.
        let (mut dev, script) = device(&[
            (&request, &reply(&[wrote(ACK_WAIT)])),
            (&request, &reply(&[wrote(ACK_OK)])),
        ]);
        write(&mut dev, CSW, 0x12).unwrap();
        assert!(script.finished());

        // After WAIT_RETRIES the driver gives up and frees the AP with DAPABORT.
        let waited = reply(&[wrote(ACK_WAIT)]);
        let abort = frame(DAPABORT);
        let ok = frame(WRITE_OK);
        let mut frames: Vec<(&[u8], &[u8])> = vec![];
        for _ in 0..=WAIT_RETRIES {
            frames.push((&request, &waited));
        }
        frames.push((&abort, &ok));
        let (mut dev, script) = device(&frames);
        let failure = write(&mut dev, CSW, 0x12).unwrap_err();
        assert_eq!(transfer_error(failure), (0, SwdTransferError::WaitResponse));
        assert!(script.finished());
    }

    #[test]
    fn a_wait_before_a_further_write_is_not_retried() {
        // The first write WAITs; the second still went out. Replaying from the WAIT would
        // reissue that second write, so a WAIT with a write still ahead is reported as is.
        let request = req(&[Op::write(DRW, 0x11), Op::write(DRW, 0x22)]);
        let (mut dev, script) = device(&[
            (&request, &reply(&[wrote(ACK_WAIT), wrote(ACK_OK)])),
            (&frame(DAPABORT), &frame(WRITE_OK)),
        ]);
        let failure = write_block(&mut dev, DRW, &[0x11, 0x22]).unwrap_err();
        assert_eq!(transfer_error(failure), (0, SwdTransferError::WaitResponse));
        assert!(script.finished());
    }

    #[test]
    fn a_block_read_wait_retries_only_the_waited_read() {
        // The first read WAITs and posts nothing; the second returns the stale posted value
        // and posts the first word, which RDBUFF fetches. Only the first read goes again.
        let (mut dev, script) = device(&[
            (
                &req(&[Op::read(DRW), Op::read(DRW), Op::read(RDBUFF)]),
                &reply(&[wait(), ok(0xDEAD_BEEF), ok(1)]),
            ),
            (
                &req(&[Op::read(DRW), Op::read(RDBUFF)]),
                &reply(&[ok(0xDEAD_BEEF), ok(2)]),
            ),
        ]);
        assert_eq!(read_block(&mut dev, DRW, 2).unwrap(), [1, 2]);
        assert!(script.finished());
    }

    #[test]
    fn a_wait_mid_batch_keeps_the_reads_accepted_after_it() {
        // Reads 1, 2 and 4 are accepted and TAR moves on with each, so the words after the
        // WAIT are already in hand; the retry fetches the one word still missing.
        let (mut dev, script) = device(&[
            (
                &req(&[
                    Op::read(DRW),
                    Op::read(DRW),
                    Op::read(DRW),
                    Op::read(DRW),
                    Op::read(RDBUFF),
                ]),
                &reply(&[ok(0xDEAD_BEEF), ok(0), wait(), ok(1), ok(2)]),
            ),
            (
                &req(&[Op::read(DRW), Op::read(RDBUFF)]),
                &reply(&[ok(0xDEAD_BEEF), ok(3)]),
            ),
        ]);
        assert_eq!(read_block(&mut dev, DRW, 4).unwrap(), [0, 1, 2, 3]);
        assert!(script.finished());
    }

    #[test]
    fn wait_on_the_rdbuff_read_is_retried() {
        // The AP read is accepted but the RDBUFF read that fetches its value WAITs, so only
        // the RDBUFF read is retried.
        let (mut dev, script) = device(&[
            (
                &req(&[Op::read(DRW), Op::read(RDBUFF)]),
                &reply(&[ok(0), wait()]),
            ),
            (&req(&[Op::read(RDBUFF)]), &reply(&[ok(0x55)])),
        ]);
        assert_eq!(read(&mut dev, DRW).unwrap(), 0x55);
        assert!(script.finished());
    }

    #[test]
    fn fault_clears_the_sticky_flags() {
        // A FAULT is reported only after CTRL/STAT is read and ABORT clears the sticky bits.
        let (mut dev, script) = device(&[
            (
                &req(&[Op::read(DPIDR)]),
                &reply(&[read_reply(ACK_FAULT, 0)]),
            ),
            (&req(&[Op::read(CTRL)]), &reply(&[ok(0x20)])),
            (&req(&[Op::write(ABORT, 0x1E)]), &frame(WRITE_OK)),
        ]);
        let failure = read(&mut dev, DPIDR).unwrap_err();
        assert_eq!(
            transfer_error(failure),
            (0, SwdTransferError::FaultResponse)
        );
        assert!(script.finished());
    }

    #[test]
    fn a_failure_keeps_the_reads_before_it() {
        // The batch stops at the second read; the first still answers.
        let (mut dev, script) = device(&[(
            &req(&[Op::read(DPIDR), Op::read(CTRL)]),
            &reply(&[ok(0x2BA0_1477), read_reply(ACK_NONE, 0)]),
        )]);
        let mut batch = SwdBatch::new();
        let dpidr = batch.read(Port::Dp, 0x0);
        let ctrl = batch.read(Port::Dp, 0x4);
        let failure = dev.run_swd_batch(&batch).unwrap_err();
        assert_eq!(failure.fault_operation, 1);
        let mut results = failure.results;
        assert_eq!(results.take(dpidr).ok(), Some(0x2BA0_1477));
        assert!(results.take(ctrl).is_err());
        assert!(script.finished());
    }

    #[test]
    fn no_target_and_bad_parity_are_reported() {
        let request = req(&[Op::read(DPIDR)]);
        // An ACK of 0b111 is no target on the line.
        let (mut dev, _) = device(&[(
            &request,
            &reply(&[vec![SUB_READ, 0x07, 0xFF, 0xFF, 0xFF, 0xFF, 0x03]]),
        )]);
        let failure = read(&mut dev, DPIDR).unwrap_err();
        assert_eq!(
            transfer_error(failure),
            (0, SwdTransferError::NoAcknowledge)
        );

        // A read whose parity bit does not match its data.
        let mut bad = ok(0x1);
        bad[6] ^= 1;
        let (mut dev, _) = device(&[(&request, &reply(&[bad]))]);
        let failure = read(&mut dev, DPIDR).unwrap_err();
        assert_eq!(
            transfer_error(failure),
            (0, SwdTransferError::IncorrectParity)
        );
    }

    #[test]
    fn misordered_reply_poisons_the_device() {
        // A write's reply where a read's was due means the stream is out of order; the
        // device refuses further commands until reopened.
        let request = req(&[Op::read(DPIDR)]);
        let (mut dev, script) =
            device(&[(&request, &frame(WRITE_OK)), (&request, &frame(WRITE_OK))]);
        for _ in 0..2 {
            assert!(matches!(
                read(&mut dev, DPIDR).unwrap_err().error,
                BatchError::Probe(DebugProbeError::ProbeSpecific(_))
            ));
        }
        assert!(!script.finished());
    }

    #[test]
    fn sequences_and_idles_go_out_alone_and_split_at_64_bits() {
        let (mut dev, script) = device(&[
            (&frame(&[0xA1, 16, 0, 0x9E, 0xE7]), &frame(&[SUB_SEQUENCE])),
            (
                &frame(&[0xA1, 64, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]),
                &frame(&[SUB_SEQUENCE]),
            ),
            (&frame(&[0xA1, 1, 0, 1]), &frame(&[SUB_SEQUENCE])),
            (&frame(&[0xA1, 8, 0, 0]), &frame(&[SUB_SEQUENCE])),
        ]);
        let mut batch = SwdBatch::new();
        batch.sequence(BitSequence::from_u64(16, 0xE79E));
        batch.sequence(BitSequence::repeat(true, 65));
        batch.sequence(BitSequence::new());
        batch.idle(8);
        dev.run_swd_batch(&batch).unwrap();
        assert!(script.finished());
    }

    #[test]
    fn pins_are_not_supported() {
        // The transfers before the pins op still go out, in order; the op itself is refused.
        let (mut dev, script) = device(&[(&req(&[Op::write(SELECT, 0)]), &frame(WRITE_OK))]);
        let mut batch = SwdBatch::new();
        batch.write(Port::Dp, 0x8, 0);
        let _ = batch.schedule(SwdOp::Pins {
            out: crate::probe::Pins(0),
            select: crate::probe::Pins(0),
            wait: Duration::ZERO,
        });
        let failure = dev.run_swd_batch(&batch).unwrap_err();
        assert_eq!(failure.fault_operation, 1);
        assert!(matches!(
            failure.error,
            BatchError::Probe(DebugProbeError::CommandNotSupportedByProbe { .. })
        ));
        assert!(script.finished());
    }

    /// Frames for a block read of `total` words, chunked by the batch budget. The bytes come
    /// from the encoder tested above; the independent checks are the batch counts and the
    /// size bound.
    fn block_read_script(clock: SwdClock, total: usize) -> Vec<(Vec<u8>, Vec<u8>)> {
        let mut frames = vec![];
        let mut done = 0;
        while done < total {
            let mut batch = Batch::accesses(clock, Some(Op::read(RDBUFF)));
            let count = batch
                .fill(std::iter::repeat_n(Op::read(DRW), total - done))
                .unwrap();
            let mut reply = read_reply(ACK_OK, 0xFFFF_FFFF);
            for i in done..done + count {
                reply.extend(read_reply(ACK_OK, i as u32));
            }
            reply.push(SUB_SEQUENCE);
            frames.push((frame(&batch.payload()), frame(&reply)));
            done += count;
        }
        frames
    }

    fn check_block_read(khz: u32, total: usize, expected_batches: usize) {
        let clock = SwdClock::new(khz, true).unwrap();
        let script = block_read_script(clock, total);
        assert_eq!(script.len(), expected_batches);
        for (request, reply) in &script {
            assert!(request.len() <= MAX_PACKET);
            assert!(reply.len() <= MAX_PACKET);
        }
        let frames: Vec<(&[u8], &[u8])> = script
            .iter()
            .map(|(w, r)| (w.as_slice(), r.as_slice()))
            .collect();
        let (mut dev, script) = scripted(CH347F_1_20, &frames);
        dev.select_protocol(WireProtocol::Swd).unwrap();
        dev.set_speed(khz).unwrap();
        let values = read_block(&mut dev, DRW, total).unwrap();
        assert!(values.iter().enumerate().all(|(i, &v)| v == i as u32));
        assert!(script.finished());
    }

    #[test]
    fn block_reads_fill_the_receive_buffer_at_full_speed() {
        // 3 header + 7 per read + 7 RDBUFF + 1 idle: 71 reads per batch.
        check_block_read(5000, 100, 2);
        check_block_read(5000, 71, 1);
        check_block_read(5000, 72, 2);
    }

    #[test]
    fn block_reads_respect_the_time_window_at_low_speed() {
        // 31 us per clock: (n + 1) * 46 + 8 clocks within 7 ms allows 3 reads per batch.
        check_block_read(33, 6, 2);
        check_block_read(33, 7, 3);
    }

    #[test]
    fn block_writes_fill_the_send_buffer() {
        let clock = SwdClock::new(5000, true).unwrap();
        let total = 60;
        let mut frames = vec![];
        let mut done = 0;
        while done < total {
            let mut batch = Batch::accesses(clock, None);
            let count = batch
                .fill((done..total).map(|i| Op::write(DRW, i as u32)))
                .unwrap();
            let mut reply = vec![];
            for _ in 0..count {
                reply.extend_from_slice(&[SUB_WRITE, ACK_OK]);
            }
            reply.push(SUB_SEQUENCE);
            frames.push((frame(&batch.payload()), frame(&reply)));
            done += count;
        }
        // 3 header + 9 per write + 4 idle: 56 writes per batch.
        assert_eq!(frames.len(), 2);
        let frames: Vec<(&[u8], &[u8])> = frames
            .iter()
            .map(|(w, r)| (w.as_slice(), r.as_slice()))
            .collect();
        let (mut dev, script) = device(&frames);
        let values: Vec<u32> = (0..total as u32).collect();
        write_block(&mut dev, DRW, &values).unwrap();
        assert!(script.finished());
    }
}
