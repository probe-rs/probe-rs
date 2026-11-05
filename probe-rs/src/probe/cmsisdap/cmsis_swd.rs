use std::collections::VecDeque;
use std::time::Duration;

use crate::{
    architecture::arm::{
        RegisterAddress,
        dp::{Abort, DpRegisterAddress},
    },
    probe::{
        BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbeError, HandleId,
        Results,
        cmsisdap::{
            CmsisDap,
            commands::{
                self,
                swj::sequence::SequenceRequest,
                transfer::{
                    Ack, RW, TransferBlockRequest, TransferBlockResponse, TransferRequest,
                    TransferResponse,
                },
            },
        },
        swd::{Direction, Pins as SwdPins, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError},
    },
};

pub(crate) fn cmsis_handles_wait(wait_retry: u16) -> bool {
    wait_retry > 0
}

/// How often an access repeats before `DAP_TransferBlock` carries it.
///
/// `DAP_TransferBlock` sends the access once for every repeat, so it needs
/// fewer packets than `DAP_Transfer`.
const MIN_BLOCK_TRANSFERS: usize = 2;

/// How many transfer requests may be outstanding at once.
///
/// Overlapping a request with the previous reply hides one round trip each time. Past a handful
/// the wait is already covered and a deeper queue only holds more requests in memory.
const MAX_PIPELINED: usize = 8;

/// Return how many words one `DAP_TransferBlock` packet holds.
pub(crate) fn block_words_per_packet(packet_size: u16) -> usize {
    // A packet holds a one byte HID report id, the command id, the DAP index,
    // two length bytes, and the request byte, before the data.
    ((packet_size as usize - 6) / 4).max(1)
}

fn transfer_data(op: &SwdOp) -> u32 {
    match op {
        SwdOp::Transfer { data, .. } => *data,
        _ => 0,
    }
}

/// Return how many operations from `start` repeat the same access.
pub(crate) fn run_length(ops: &[(HandleId, SwdOp)], start: usize) -> usize {
    let SwdOp::Transfer {
        port,
        addr,
        direction,
        ..
    } = ops[start].1
    else {
        return 0;
    };

    ops[start..]
        .iter()
        .take_while(|(_, op)| {
            matches!(
                op,
                SwdOp::Transfer {
                    port: run_port,
                    addr: run_addr,
                    direction: run_direction,
                    ..
                } if *run_port == port && *run_addr == addr && *run_direction == direction
            )
        })
        .count()
}

/// Split a run of transfers into the packets that carry them.
///
/// Each packet arrives with its request built and its transfers carrying the batch index a fault
/// is reported against. `run_start` is where the run sits in the batch.
fn split_into_packets(
    ops: &[(HandleId, SwdOp)],
    run_start: usize,
    dap_index: u8,
    packet_size: u16,
) -> impl Iterator<Item = PendingBatch> {
    let mut offset = 0;

    std::iter::from_fn(move || {
        let mut packet = PendingBatch::new(dap_index);

        while let Some((_, op)) = ops.get(offset) {
            let SwdOp::Transfer { direction, .. } = op else {
                break;
            };

            // A repeat of one access can go as a block transfer, which is pipelined and, for a
            // write, denser -- but only if it gets a packet to itself. Ask for the whole run: if
            // it fits here, whatever follows can still share the packet; if it does not, the
            // block command takes it.
            let run = run_length(ops, offset);
            let wanted = if run >= MIN_BLOCK_TRANSFERS { run } else { 1 };

            let run_items =
                ops[offset..offset + wanted]
                    .iter()
                    .enumerate()
                    .map(|(index, (id, op))| {
                        let SwdOp::Transfer {
                            port, addr, data, ..
                        } = op
                        else {
                            unreachable!("a run holds only transfers")
                        };
                        let address = CmsisDap::swd_register(*port, *addr);
                        (id.clone(), address, *data, run_start + offset + index)
                    });

            if !packet.push_all(*direction, packet_size, run_items) {
                break;
            }
            offset += wanted;
        }

        (!packet.is_empty()).then_some(packet)
    })
}

struct PendingTransfer {
    id: HandleId,
    direction: Direction,
    batch_index: usize,
}

/// Transfers queued for one `DAP_Transfer`, and what its reply maps back to.
///
/// The two halves stay in step: entry `n` of `transfers` describes transfer `n` of `request`, which
/// is what lets a reply be matched to the batch operation that asked for it. Only
/// [`Self::push_all`] adds to either.
struct PendingBatch {
    request: TransferRequest,
    transfers: Vec<PendingTransfer>,
    /// Whether the packet writes a register, and so [waits](InFlight::waits).
    writes: bool,
}

impl PendingBatch {
    fn new(dap_index: u8) -> Self {
        let mut request = TransferRequest::empty();
        request.dap_index = dap_index;
        Self {
            request,
            transfers: Vec::new(),
            writes: false,
        }
    }

    fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    /// Push every transfer `items` yields, or none of them when the packet runs out of room.
    ///
    /// Returns whether they all fit. Every transfer of a run has the same size, so whether the run
    /// fits is settled before any of it is added: it either travels in this packet or waits for
    /// the next one intact.
    ///
    /// An empty packet always takes one transfer however small the packet is, or splitting a batch
    /// would never make progress.
    fn push_all<I>(&mut self, direction: Direction, packet_size: u16, items: I) -> bool
    where
        I: IntoIterator<Item = (HandleId, RegisterAddress, u32, usize)>,
        I::IntoIter: ExactSizeIterator,
    {
        let items = items.into_iter();
        let count = items.len();
        let forced = self.is_empty() && count == 1;
        if !forced
            && !self
                .request
                .has_room_for_many(Self::rw(direction), count, packet_size)
        {
            return false;
        }

        self.writes |= direction == Direction::Write;
        for (id, address, data, batch_index) in items {
            match direction {
                Direction::Read => self.request.add_read(address),
                Direction::Write => self.request.add_write(address, data),
            }
            self.transfers.push(PendingTransfer {
                id,
                direction,
                batch_index,
            });
        }
        true
    }

    fn rw(direction: Direction) -> RW {
        match direction {
            Direction::Read => RW::R,
            Direction::Write => RW::W,
        }
    }
}

/// A `DAP_TransferBlock` packet: one chunk of a repeated access.
struct PendingBlock<'a> {
    /// The batch index of the chunk's first operation.
    chunk_start: usize,
    chunk: &'a [(HandleId, SwdOp)],
    request: TransferBlockRequest,
    direction: Direction,
    /// Whether an earlier chunk of the same run went before it.
    continues_run: bool,
}

/// A transfer request sent and not yet answered, and what its reply maps back to.
enum InFlight<'a> {
    Transfer(PendingBatch),
    Block(PendingBlock<'a>),
}

impl InFlight<'_> {
    /// The batch index of the first operation the request carries.
    fn first_operation(&self) -> usize {
        match self {
            InFlight::Transfer(packet) => packet.transfers[0].batch_index,
            InFlight::Block(block) => block.chunk_start,
        }
    }

    /// Whether the request has to wait until every earlier reply is in.
    ///
    /// A request sent ahead still runs when one before it failed with WAIT or no ACK, which set
    /// no sticky flag. A read that runs then is not captured, but a write takes effect, at an
    /// address that a failed TAR write before it may have left wrong. So a request that writes
    /// waits, except a later chunk of a block run, which goes on from the chunk before it as it
    /// did before requests were pipelined across packets.
    fn waits(&self) -> bool {
        match self {
            InFlight::Transfer(packet) => packet.writes,
            InFlight::Block(block) => block.direction == Direction::Write && !block.continues_run,
        }
    }
}

/// Transfer requests outstanding at the probe, oldest first, and what has come back so far.
struct Pipeline<'a> {
    in_flight: VecDeque<InFlight<'a>>,
    depth: usize,
    /// The first failure in batch order, and the operation it is reported against.
    failure: Option<(BatchError<DebugProbeError>, usize)>,
    results: Results,
}

impl Pipeline<'_> {
    fn new(depth: usize, results: Results) -> Self {
        Self {
            in_flight: VecDeque::with_capacity(depth),
            depth,
            failure: None,
            results,
        }
    }

    /// Record a failure. The one at the lowest operation is kept.
    ///
    /// A failed send is recorded while replies before it are still in flight, so a later failure
    /// can be an earlier operation's.
    fn fail(&mut self, failure: (BatchError<DebugProbeError>, usize)) {
        if self.failure.as_ref().is_none_or(|(_, at)| failure.1 < *at) {
            self.failure = Some(failure);
        }
    }

    /// Whether the results of a reply from `operation` on are the caller's: only if no failure
    /// before it has been recorded.
    fn captures(&self, operation: usize) -> bool {
        self.failure.as_ref().is_none_or(|(_, at)| operation < *at)
    }
}

impl CmsisDap {
    fn swd_register(port: Port, addr: u8) -> RegisterAddress {
        match port {
            Port::Dp => RegisterAddress::DpRegister(DpRegisterAddress {
                address: addr,
                bank: None,
            }),
            Port::Ap => RegisterAddress::ApRegister(addr),
        }
    }

    fn ack_to_transfer_error(ack: Ack) -> SwdTransferError {
        match ack {
            Ack::Ok => SwdTransferError::Protocol,
            Ack::Wait => SwdTransferError::WaitResponse,
            Ack::Fault => SwdTransferError::FaultResponse,
            Ack::NoAck => SwdTransferError::NoAcknowledge,
        }
    }

    /// Classify one `DAP_Transfer` reply.
    ///
    /// Pure, like [`Self::classify_block_response`], so recovery can wait until the reply has
    /// been taken. Returns the batch index of the transfer the reply blames.
    ///
    /// The acknowledgement is classified before the transfer count. A FAULT or WAIT stops the
    /// probe early, so a short count is the *consequence* of that error, not evidence that the
    /// probe failed to report one. Checking the count first hides the acknowledgement and skips
    /// recovery, leaving CTRL/STAT.STICKYERR set - after which the DP faults every subsequent
    /// AP access (IHI0031G B4.2.4) and the session cannot recover on its own.
    fn classify_transfer_response(
        response: &TransferResponse,
        transfers: &[PendingTransfer],
    ) -> Result<(), (BatchError<DebugProbeError>, usize)> {
        let count = response.transfers.len();
        let fault_operation = transfers[count.min(transfers.len()).saturating_sub(1)].batch_index;

        if response.last_transfer_response.protocol_error {
            return Err((
                BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::Protocol)),
                fault_operation,
            ));
        }

        match response.last_transfer_response.ack {
            Ack::Ok => {}
            ack => {
                return Err((
                    BatchError::Specific(DebugProbeError::SwdTransfer(
                        Self::ack_to_transfer_error(ack),
                    )),
                    fault_operation,
                ));
            }
        }

        // An OK acknowledgement with a short count really is the probe not saying why.
        if count < transfers.len() {
            return Err((
                BatchError::Probe(DebugProbeError::Other(format!(
                    "Possible error in CMSIS-DAP probe: Only {}/{} transfers were executed, but no error was reported.",
                    count,
                    transfers.len()
                ))),
                fault_operation,
            ));
        }

        Ok(())
    }

    fn max_words_per_block_packet(&self) -> usize {
        block_words_per_packet(self.packet_size)
    }

    /// Classify one `DAP_TransferBlock` reply.
    ///
    /// Takes no `self`, so it cannot issue a command. Whatever the reply says, recovery has to
    /// wait until every outstanding reply has been taken.
    fn classify_block_response(
        response: &TransferBlockResponse,
        chunk_len: usize,
        chunk_start: usize,
    ) -> Result<(), (BatchError<DebugProbeError>, usize)> {
        let count = usize::from(response.transfer_count);
        let fault_operation = chunk_start + count.min(chunk_len).saturating_sub(1);

        if response.transfer_response.protocol_error {
            return Err((
                BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::Protocol)),
                fault_operation,
            ));
        }

        match response.transfer_response.ack {
            Ack::Ok => {}
            ack => {
                return Err((
                    BatchError::Specific(DebugProbeError::SwdTransfer(
                        Self::ack_to_transfer_error(ack),
                    )),
                    fault_operation,
                ));
            }
        }

        if count < chunk_len {
            return Err((
                BatchError::Probe(DebugProbeError::Other(format!(
                    "Possible error in CMSIS-DAP probe: Only {}/{} transfers were executed, but no error was reported.",
                    count, chunk_len
                ))),
                fault_operation,
            ));
        }

        Ok(())
    }

    /// Put the port back in order after a block transfer failed.
    ///
    /// Each arm is a command in its own right, which is why this is separate from classifying the
    /// reply: issuing one while the probe still owes replies leaves the two out of step.
    fn recover_from_block_error(&mut self, error: &BatchError<DebugProbeError>) {
        match error {
            BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::FaultResponse)) => {
                if let Err(error) = self.handle_sticky_err() {
                    tracing::warn!("Failed to clear the sticky error: {error}");
                }
            }
            BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse)) => {
                let abort = {
                    let mut abort = Abort(0);
                    abort.set_dapabort(true);
                    abort
                };
                if let Err(error) = self.write_abort(abort) {
                    tracing::warn!("Failed to abort the transfer: {error}");
                }
            }
            _ => {}
        }
    }

    /// How many transfer requests to keep outstanding.
    ///
    /// Leaves one of the probe's packet buffers free: a probe with all of them full does not take
    /// `DAP_TransferAbort` before the current transfer ends.
    fn pipeline_depth(&self) -> usize {
        (self.packet_count as usize)
            .saturating_sub(1)
            .clamp(1, MAX_PIPELINED)
    }

    /// Send `command`, first taking replies until the pipeline has room for it, or every reply
    /// if it [waits](InFlight::waits).
    ///
    /// Returns whether it was sent. Nothing is sent once a command has failed.
    fn pipeline_send<'a>(&mut self, pipeline: &mut Pipeline<'a>, command: InFlight<'a>) -> bool {
        let room = if command.waits() { 1 } else { pipeline.depth };
        while pipeline.failure.is_none() && pipeline.in_flight.len() >= room {
            self.pipeline_take(pipeline);
        }
        if pipeline.failure.is_some() {
            return false;
        }

        let sent = match &command {
            InFlight::Transfer(packet) => commands::send_request(&mut self.device, &packet.request),
            InFlight::Block(block) => commands::send_request(&mut self.device, &block.request),
        };
        match sent {
            Ok(()) => {
                pipeline.in_flight.push_back(command);
                true
            }
            // Replies already in flight are still the caller's.
            Err(error) => {
                pipeline.fail((
                    BatchError::Probe(DebugProbeError::from(error)),
                    command.first_operation(),
                ));
                false
            }
        }
    }

    /// Send a repeated access as `DAP_TransferBlock` packets, and return whether all were sent.
    fn pipeline_send_block<'a>(
        &mut self,
        pipeline: &mut Pipeline<'a>,
        run: &'a [(HandleId, SwdOp)],
        run_start: usize,
    ) -> bool {
        let SwdOp::Transfer {
            port,
            addr,
            direction,
            ..
        } = run[0].1
        else {
            return true;
        };
        let address = Self::swd_register(port, addr);
        let words_per_packet = self.max_words_per_block_packet();

        for (chunk_index, chunk) in run.chunks(words_per_packet).enumerate() {
            let mut request = match direction {
                Direction::Read => TransferBlockRequest::read_request(address, chunk.len() as u16),
                Direction::Write => TransferBlockRequest::write_request(
                    address,
                    chunk.iter().map(|(_, op)| transfer_data(op)).collect(),
                ),
            };
            request.dap_index = self.jtag_state.chain_params.index as u8;
            let command = InFlight::Block(PendingBlock {
                chunk_start: run_start + chunk_index * words_per_packet,
                chunk,
                request,
                direction,
                continues_run: chunk_index > 0,
            });
            if !self.pipeline_send(pipeline, command) {
                return false;
            }
        }
        true
    }

    /// Take the reply to the oldest command in flight.
    fn pipeline_take(&mut self, pipeline: &mut Pipeline<'_>) {
        let Some(command) = pipeline.in_flight.pop_front() else {
            return;
        };

        let received = match &command {
            InFlight::Transfer(packet) => self.take_transfer_reply(pipeline, packet),
            InFlight::Block(block) => self.take_block_reply(pipeline, block),
        };
        if let Err(error) = received {
            // A late reply arrives ahead of the replies still in flight, so reading on would
            // give each request the reply of the one before it.
            if commands::may_be_out_of_step(&error) || !pipeline.in_flight.is_empty() {
                self.device.resynchronise();
            }
            pipeline.in_flight.clear();
            pipeline.fail((
                BatchError::Probe(DebugProbeError::from(error)),
                command.first_operation(),
            ));
        }
    }

    /// Take a `DAP_Transfer` reply and keep the values it read.
    ///
    /// Returns an error only when no reply could be read. A reply that reports a failure is
    /// recorded in `pipeline`.
    fn take_transfer_reply(
        &mut self,
        pipeline: &mut Pipeline<'_>,
        packet: &PendingBatch,
    ) -> Result<(), commands::CmsisDapError> {
        let response = commands::receive_response(&mut self.device, &packet.request)?;
        if let Err(failure) = Self::classify_transfer_response(&response, &packet.transfers) {
            pipeline.fail(failure);
            return Ok(());
        }
        if !pipeline.captures(packet.transfers[0].batch_index) {
            return Ok(());
        }

        for (transfer, response_transfer) in packet.transfers.iter().zip(response.transfers.iter())
        {
            if transfer.direction != Direction::Read || !transfer.id.should_capture() {
                continue;
            }
            let Some(data) = response_transfer.data else {
                pipeline.fail((
                    BatchError::Probe(DebugProbeError::Other(
                        "CMSIS-DAP read did not return any data".to_string(),
                    )),
                    transfer.batch_index,
                ));
                return Ok(());
            };
            pipeline
                .results
                .push(&transfer.id, CommandResult::U32(data));
        }
        Ok(())
    }

    /// Take a `DAP_TransferBlock` reply and keep the values it read, as
    /// [`Self::take_transfer_reply`] does.
    fn take_block_reply(
        &mut self,
        pipeline: &mut Pipeline<'_>,
        block: &PendingBlock<'_>,
    ) -> Result<(), commands::CmsisDapError> {
        let response = commands::receive_response(&mut self.device, &block.request)?;
        if let Err(failure) =
            Self::classify_block_response(&response, block.chunk.len(), block.chunk_start)
        {
            pipeline.fail(failure);
            return Ok(());
        }
        if !pipeline.captures(block.chunk_start) || block.direction != Direction::Read {
            return Ok(());
        }

        for ((id, _), value) in block.chunk.iter().zip(response.transfer_data.iter()) {
            if id.should_capture() {
                pipeline.results.push(id, CommandResult::U32(*value));
            }
        }
        Ok(())
    }

    /// Take every outstanding reply, and return whether no command has failed.
    fn pipeline_wait(&mut self, pipeline: &mut Pipeline<'_>) -> bool {
        while !pipeline.in_flight.is_empty() {
            self.pipeline_take(pipeline);
        }
        pipeline.failure.is_none()
    }

    /// Send the transfers from `start` on, up to the next operation of another kind, and return
    /// how many were sent, or `None` if a command failed.
    fn pipeline_send_transfers<'a>(
        &mut self,
        pipeline: &mut Pipeline<'a>,
        ops: &'a [(HandleId, SwdOp)],
        start: usize,
    ) -> Option<usize> {
        let dap_index = self.jtag_state.chain_params.index as u8;
        let mut sent = 0;
        for packet in split_into_packets(&ops[start..], start, dap_index, self.packet_size) {
            sent += packet.transfers.len();
            if !self.pipeline_send(pipeline, InFlight::Transfer(packet)) {
                return None;
            }
        }
        if sent > 0 {
            return Some(sent);
        }

        // A repeat too long for a packet of its own goes as block transfers.
        let run = run_length(ops, start);
        self.pipeline_send_block(pipeline, &ops[start..start + run], start)
            .then_some(run)
    }

    /// Take every outstanding reply, then report the first failure in batch order.
    fn pipeline_finish(
        &mut self,
        mut pipeline: Pipeline<'_>,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        self.pipeline_wait(&mut pipeline);
        match pipeline.failure {
            Some((error, fault_operation)) => {
                self.recover_from_block_error(&error);
                Err(BatchExecutionError {
                    error,
                    results: pipeline.results,
                    fault_operation,
                })
            }
            None => Ok(pipeline.results),
        }
    }

    fn run_swj_sequence_op(&mut self, bits: &BitSequence) -> Result<(), DebugProbeError> {
        self.connect_if_needed()?;

        const MAX_BITS: usize = 256;
        let mut offset = 0;
        while offset < bits.len() {
            let chunk_len = (bits.len() - offset).min(MAX_BITS);
            let mut data = vec![0u8; chunk_len.div_ceil(8)];
            for i in 0..chunk_len {
                if bits[offset + i] {
                    data[i / 8] |= 1 << (i % 8);
                }
            }
            let bit_count = if chunk_len == MAX_BITS {
                0
            } else {
                chunk_len as u8
            };
            self.send_swj_sequences(SequenceRequest::new(&data, bit_count)?)
                .map_err(DebugProbeError::from)?;
            offset += chunk_len;
        }

        Ok(())
    }

    fn run_swj_idle(&mut self, cycles: u32) -> Result<(), DebugProbeError> {
        let mut remaining = cycles as usize;
        while remaining > 0 {
            let chunk_len = remaining.min(256);
            let data = vec![0u8; chunk_len.div_ceil(8)];
            let bit_count = if chunk_len == 256 { 0 } else { chunk_len as u8 };
            self.send_swj_sequences(SequenceRequest::new(&data, bit_count)?)
                .map_err(DebugProbeError::from)?;
            remaining -= chunk_len;
        }
        Ok(())
    }

    fn run_swj_pins_op(
        &mut self,
        out: SwdPins,
        select: SwdPins,
        wait: Duration,
    ) -> Result<SwdPins, DebugProbeError> {
        self.reset_line.check_pins(out, select)?;
        self.connect_if_needed()?;

        let request = commands::swj::pins::SWJPinsRequest::from_raw_values(
            out.0,
            select.0,
            wait.as_micros() as u32,
        );
        let levels = commands::send_command(&mut self.device, &request)?;
        Ok(SwdPins(levels.0))
    }
}

impl SwdProbe for CmsisDap {
    /// Run a batch with its `DAP_Transfer` and `DAP_TransferBlock` packets in one queue of
    /// outstanding requests.
    ///
    /// Reads are sent ahead of the replies before them. After a failure the ones behind it may
    /// still run, and their values are not captured. A request that writes waits for every
    /// earlier reply (see [`InFlight::waits`]), as do sequence, idle and pins operations.
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let ops: Vec<(HandleId, SwdOp)> = batch
            .iter()
            .map(|(id, op)| (id.clone(), op.clone()))
            .collect();
        let mut pipeline = Pipeline::new(self.pipeline_depth(), Results::new());

        let mut batch_index = 0;
        while let Some((id, op)) = ops.get(batch_index) {
            // Anything but a transfer is a command of its own: everything before it has to have
            // run.
            if !matches!(op, SwdOp::Transfer { .. }) && !self.pipeline_wait(&mut pipeline) {
                break;
            }
            let outcome = match op {
                SwdOp::Transfer { .. } => {
                    match self.pipeline_send_transfers(&mut pipeline, &ops, batch_index) {
                        Some(sent) => batch_index += sent,
                        None => break,
                    }
                    continue;
                }
                SwdOp::Sequence(bits) => self.run_swj_sequence_op(bits).map(|()| None),
                SwdOp::Idle { cycles } => self.run_swj_idle(*cycles).map(|()| None),
                SwdOp::Pins { out, select, wait } => {
                    self.run_swj_pins_op(*out, *select, *wait).map(Some)
                }
            };
            match outcome {
                Ok(Some(levels)) if id.should_capture() => {
                    pipeline.results.push(id, CommandResult::U8(levels.0));
                }
                Ok(_) => {}
                Err(error) => {
                    return Err(BatchExecutionError::new_from_debug_probe_at(
                        error,
                        pipeline.results,
                        batch_index,
                    ));
                }
            }

            batch_index += 1;
        }

        self.pipeline_finish(pipeline)
    }

    fn handles_wait(&self) -> bool {
        cmsis_handles_wait(self.transfer_wait_retry)
    }

    fn handles_ap_pipeline(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::{
        cmsisdap::commands::{Request, transfer::LastTransferResponse},
        swd::{Direction, Port, SwdOp},
    };

    fn block_response(count: u16, ack: Ack) -> TransferBlockResponse {
        TransferBlockResponse {
            transfer_count: count,
            transfer_response: LastTransferResponse {
                ack,
                protocol_error: false,
                _value_mismatch: false,
            },
            transfer_data: Vec::new(),
        }
    }

    #[test]
    fn a_late_block_reply_does_not_reach_the_next_command() {
        use crate::probe::cmsisdap::commands::{
            CmsisDapDevice, CommandId,
            fake::{FakeProbe, Reply, standard_reply},
            general::info::PacketCountCommand,
        };

        let mut blocks = 0;
        let probe = FakeProbe::shared(64, move |command| {
            if command[0] != CommandId::TransferBlock.id() {
                return standard_reply(command, 64, 4);
            }
            let count = u16::from_le_bytes([command[2], command[3]]);
            let mut reply = vec![command[0], command[2], command[3], Ack::Ok as u8];
            for word in 0..u32::from(count) {
                reply.extend((blocks * 100 + word).to_le_bytes());
            }
            blocks += 1;
            // The probe is slow on the second block, so its reply and the third come late.
            if blocks == 2 {
                Reply::Held(reply)
            } else {
                Reply::Now(reply)
            }
        });
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();

        let mut batch = SwdBatch::new();
        let reads: Vec<_> = (0..30).map(|_| batch.read(Port::Ap, 0xC)).collect();

        let error = dap.run_batch(&batch).unwrap_err();
        let words_per_packet = dap.max_words_per_block_packet();
        assert_eq!(error.fault_operation, words_per_packet);
        let mut results = error.results;
        for (word, read) in reads.into_iter().take(words_per_packet).enumerate() {
            assert_eq!(results.take(read).ok(), Some(word as u32));
        }

        // One resynchronisation, without a wait for the third reply.
        let aborts = probe.lock().unwrap().aborts();
        assert_eq!(aborts, 1);

        let packet_count = commands::send_command(&mut dap.device, &PacketCountCommand {});
        assert_eq!(packet_count.unwrap(), 4);
    }

    /// How `counting_probe` fails.
    #[derive(Clone, Copy, Default)]
    struct Fail {
        /// The nth `DAP_TransferBlock`, counting from one, answers WAIT with nothing done.
        block: Option<usize>,
        /// Every read in a `DAP_Transfer` from the nth on, counting from zero, answers FAULT, as
        /// a DP with a sticky flag set does.
        fault_from: Option<u32>,
        /// The nth `DAP_Transfer`, counting from one, does not reach the probe.
        send: Option<usize>,
    }

    /// A probe with 64 byte packets and four buffers that answers transfers with OK, each read
    /// with the next number, unless `fail` says otherwise.
    fn counting_probe(
        fail: Fail,
    ) -> std::sync::Arc<std::sync::Mutex<crate::probe::cmsisdap::commands::fake::FakeProbe>> {
        use crate::probe::cmsisdap::commands::{
            CommandId,
            fake::{FakeProbe, Reply, standard_reply},
        };
        let mut next = 0u32;
        let mut blocks = 0;
        let mut transfers = 0;
        FakeProbe::shared(64, move |command| {
            if command[0] == CommandId::Transfer.id() {
                transfers += 1;
                if fail.send == Some(transfers) {
                    return Reply::WriteFails;
                }
                let count = command[2] as usize;
                let mut reply = vec![command[0], 0, Ack::Ok as u8];
                let mut position = 3;
                for _ in 0..count {
                    let request = command[position];
                    position += 1;
                    if request & 0x02 == 0 {
                        position += 4;
                    } else if fail.fault_from.is_some_and(|from| next >= from) {
                        reply[2] = Ack::Fault as u8;
                        break;
                    } else {
                        reply.extend(next.to_le_bytes());
                        next += 1;
                    }
                    reply[1] += 1;
                }
                Reply::Now(reply)
            } else if command[0] == CommandId::TransferBlock.id() {
                blocks += 1;
                if fail.block == Some(blocks) {
                    return Reply::Now(vec![command[0], 0, 0, Ack::Wait as u8]);
                }
                let count = u16::from_le_bytes([command[2], command[3]]);
                let mut reply = vec![command[0], command[2], command[3], Ack::Ok as u8];
                if command[4] & 0x02 != 0 {
                    for _ in 0..count {
                        reply.extend(next.to_le_bytes());
                        next += 1;
                    }
                }
                Reply::Now(reply)
            } else {
                standard_reply(command, 64, 4)
            }
        })
    }

    /// For every transfer command sent after opening the probe: its command ID, whether it
    /// writes, and how many replies the probe owed when it arrived.
    ///
    /// Read out before asserting: a failed assertion with the lock held poisons it, and the drop
    /// of the `CmsisDap` then panics on it, which aborts the test binary.
    fn transfers_sent(
        probe: &std::sync::Mutex<crate::probe::cmsisdap::commands::fake::FakeProbe>,
        opened: usize,
    ) -> Vec<(u8, bool, usize)> {
        use crate::probe::cmsisdap::commands::CommandId;

        let probe = probe.lock().unwrap();
        let (transfer, block) = (CommandId::Transfer.id(), CommandId::TransferBlock.id());
        probe.sent[opened..]
            .iter()
            .zip(&probe.owed_at_write[opened..])
            .filter(|(command, _)| command[0] == transfer || command[0] == block)
            .map(|(command, owed)| {
                let writes = if command[0] == block {
                    command[4] & 0x02 == 0
                } else {
                    // Walk the requests: a write carries four bytes of data.
                    let (mut position, mut writes) = (3, false);
                    for _ in 0..command[2] {
                        if command[position] & 0x02 == 0 {
                            writes = true;
                            position += 4;
                        }
                        position += 1;
                    }
                    writes
                };
                (command[0], writes, *owed)
            })
            .collect()
    }

    #[test]
    fn reads_go_ahead_of_earlier_replies_but_a_write_waits_for_them() {
        use crate::probe::cmsisdap::commands::CmsisDapDevice;

        let probe = counting_probe(Fail::default());
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        let opened = probe.lock().unwrap().sent.len();

        // Two pages of a memory read: TAR, then more DRW reads than one packet holds, twice; then
        // a SELECT write and one more read.
        let mut batch = SwdBatch::new();
        let mut reads = Vec::new();
        for page in 0..2u32 {
            batch.write(Port::Ap, 0x4, page * 1024);
            reads.extend((0..20).map(|_| batch.read(Port::Ap, 0xC)));
        }
        batch.write(Port::Dp, 0x8, 0);
        reads.push(batch.read(Port::Ap, 0xC));

        let mut results = dap.run_batch(&batch).unwrap();
        for (expected, read) in reads.into_iter().enumerate() {
            assert_eq!(results.take(read).ok(), Some(expected as u32));
        }

        let sent = transfers_sent(&probe, opened);
        let owes_nothing = probe.lock().unwrap().owes_nothing();
        // The TAR writes and the SELECT write went out with nothing owed.
        let writes: Vec<_> = sent.iter().filter(|(_, writes, _)| *writes).collect();
        assert_eq!(writes.len(), 3);
        assert!(writes.iter().all(|(_, _, owed)| *owed == 0), "{sent:?}");
        // The blocks of reads went out behind the TAR write before them, one buffer short of the
        // probe's four.
        let most_owed = sent.iter().map(|(_, _, owed)| *owed).max();
        assert_eq!(most_owed, Some(2), "{sent:?}");
        assert!(owes_nothing);
    }

    #[test]
    fn a_page_of_writes_waits_for_its_tar_write_and_its_chunks_go_ahead() {
        use crate::probe::cmsisdap::commands::{CmsisDapDevice, CommandId};

        let probe = counting_probe(Fail::default());
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        let opened = probe.lock().unwrap().sent.len();

        let words = block_words_per_packet(64);
        let mut batch = SwdBatch::new();
        for page in 0..2u32 {
            batch.write(Port::Ap, 0x4, page * 1024);
            for word in 0..2 * words as u32 {
                batch.write(Port::Ap, 0xC, word);
            }
        }
        dap.run_batch(&batch).unwrap();

        let sent = transfers_sent(&probe, opened);
        let owes_nothing = probe.lock().unwrap().owes_nothing();
        // Each page: its TAR write with nothing owed, its first chunk once the TAR write is
        // answered, and its second chunk behind the first.
        let block = CommandId::TransferBlock.id();
        let owed: Vec<_> = sent.iter().map(|(_, _, owed)| *owed).collect();
        let kinds: Vec<_> = sent.iter().map(|(id, _, _)| *id == block).collect();
        assert_eq!(kinds, [false, true, true, false, true, true], "{sent:?}");
        assert_eq!(owed, [0, 0, 1, 0, 0, 1], "{sent:?}");
        assert!(owes_nothing);
    }

    #[test]
    fn a_failing_block_drops_the_replies_queued_behind_it_and_no_write_follows_it() {
        use crate::probe::cmsisdap::commands::{CmsisDapDevice, CommandId};

        // Two pages of a memory read, each a TAR write and three blocks of DRW reads. The first
        // page's second block fails while its third is queued behind it.
        let words = block_words_per_packet(64);
        let probe = counting_probe(Fail {
            block: Some(2),
            ..Fail::default()
        });
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        let opened = probe.lock().unwrap().sent.len();

        let mut batch = SwdBatch::new();
        let mut reads = Vec::new();
        for page in 0..2u32 {
            batch.write(Port::Ap, 0x4, page * 1024);
            reads.extend((0..3 * words).map(|_| batch.read(Port::Ap, 0xC)));
        }
        let error = dap.run_batch(&batch).unwrap_err();

        // Reported against the first read of the failed block, which is operation words + 1.
        assert!(matches!(
            error.error,
            BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse))
        ));
        assert_eq!(error.fault_operation, words + 1);
        // Only the first block's reads are results, not the third's that came back after.
        let mut results = error.results;
        for (word, read) in reads.into_iter().enumerate() {
            let expected = (word < words).then_some(word as u32);
            assert_eq!(results.take(read).ok(), expected, "read {word}");
        }

        let sent = transfers_sent(&probe, opened);
        let owes_nothing = probe.lock().unwrap().owes_nothing();
        // The third block went out before the failure came back, the second page's TAR write
        // not at all. The DAPABORT write after the batch follows.
        let block = CommandId::TransferBlock.id();
        assert_eq!(sent.iter().filter(|(id, _, _)| *id == block).count(), 3);
        assert_eq!(sent.iter().filter(|(_, writes, _)| *writes).count(), 2);
        assert!(owes_nothing);
    }

    /// Runs 40 reads that alternate between two AP registers, so that `DAP_Transfer` packets of 15
    /// carry them, on a probe that fails as `fail` says. Returns the failure and how many results
    /// came back.
    fn fail_scattered_reads(fail: Fail) -> (BatchExecutionError<DebugProbeError>, usize) {
        use crate::probe::cmsisdap::commands::CmsisDapDevice;

        let probe = counting_probe(fail);
        let mut dap = CmsisDap::new_from_device(CmsisDapDevice::Fake(probe.clone())).unwrap();
        let opened = probe.lock().unwrap().sent.len();

        let mut batch = SwdBatch::new();
        let reads: Vec<_> = (0..40)
            .map(|read| batch.read(Port::Ap, if read % 2 == 0 { 0x0 } else { 0x4 }))
            .collect();
        let mut error = dap.run_batch(&batch).unwrap_err();

        let mut captured = 0;
        for (word, read) in reads.into_iter().enumerate() {
            if let Ok(value) = error.results.take(read) {
                assert_eq!(value, word as u32);
                captured += 1;
            }
        }
        let sent = transfers_sent(&probe, opened);
        let owes_nothing = probe.lock().unwrap().owes_nothing();
        assert!(owes_nothing);
        // Every packet but the last was sent ahead of the reply before it.
        assert!(sent.iter().any(|(_, _, owed)| *owed > 0), "{sent:?}");
        (error, captured)
    }

    fn fault_from(read: u32) -> Fail {
        Fail {
            fault_from: Some(read),
            ..Fail::default()
        }
    }

    #[test]
    fn a_failing_transfer_drops_the_replies_queued_behind_it() {
        // The second packet faults at its sixth read, with the third packet queued behind it.
        let (error, captured) = fail_scattered_reads(fault_from(20));
        assert!(matches!(
            error.error,
            BatchError::Specific(DebugProbeError::SwdTransfer(
                SwdTransferError::FaultResponse
            ))
        ));
        assert_eq!(error.fault_operation, 19);
        assert_eq!(captured, 15);
    }

    #[test]
    fn a_failed_send_keeps_the_replies_before_it() {
        let (error, captured) = fail_scattered_reads(Fail {
            send: Some(2),
            ..Fail::default()
        });
        // Reported against the first read of the packet that was not sent.
        assert!(
            matches!(error.error, BatchError::Probe(_)),
            "{:?}",
            error.error
        );
        assert_eq!(error.fault_operation, 15);
        assert_eq!(captured, 15);
    }

    #[test]
    fn a_fault_before_a_failed_send_is_the_one_reported() {
        // The second packet faults while its reply is still in flight when the third fails to
        // send.
        let (error, captured) = fail_scattered_reads(Fail {
            send: Some(3),
            ..fault_from(20)
        });
        assert!(matches!(
            error.error,
            BatchError::Specific(DebugProbeError::SwdTransfer(
                SwdTransferError::FaultResponse
            ))
        ));
        assert_eq!(error.fault_operation, 19);
        assert_eq!(captured, 15);
    }

    #[test]
    fn a_complete_block_reply_is_a_success() {
        let response = block_response(4, Ack::Ok);
        assert!(CmsisDap::classify_block_response(&response, 4, 100).is_ok());
    }

    #[test]
    fn a_fault_names_the_transfer_that_failed() {
        // Two of the four ran, so the second is the one that faulted.
        let response = block_response(2, Ack::Fault);
        let (error, fault_operation) =
            CmsisDap::classify_block_response(&response, 4, 100).unwrap_err();

        assert!(matches!(
            error,
            BatchError::Specific(DebugProbeError::SwdTransfer(
                SwdTransferError::FaultResponse
            ))
        ));
        assert_eq!(fault_operation, 101);
    }

    #[test]
    fn a_short_count_with_no_error_is_reported_against_the_probe() {
        let response = block_response(3, Ack::Ok);
        let (error, fault_operation) =
            CmsisDap::classify_block_response(&response, 4, 100).unwrap_err();

        assert!(matches!(
            error,
            BatchError::Probe(DebugProbeError::Other(_))
        ));
        assert_eq!(fault_operation, 102);
    }

    #[test]
    fn a_count_past_the_chunk_stays_inside_it() {
        // A probe claiming more transfers than it was given must not point outside the chunk.
        let response = block_response(9, Ack::Fault);
        let (_, fault_operation) =
            CmsisDap::classify_block_response(&response, 4, 100).unwrap_err();

        assert_eq!(fault_operation, 103);
    }

    /// How many `rw` transfers one packet holds.
    fn transfers_that_fit(rw: RW, packet_size: u16) -> usize {
        let mut request = TransferRequest::empty();
        while request.has_room_for_many(rw, 1, packet_size) {
            match rw {
                RW::R => request.add_read(RegisterAddress::ApRegister(0)),
                RW::W => request.add_write(RegisterAddress::ApRegister(0), 0),
            }
        }
        request.len()
    }

    #[test]
    fn a_packet_holds_more_reads_than_writes() {
        // A write spends five bytes of the command; a read spends one there and four in the reply.
        assert_eq!(transfers_that_fit(RW::W, 64), 12);
        assert_eq!(transfers_that_fit(RW::R, 64), 15);
        assert_eq!(transfers_that_fit(RW::W, 1024), 204);
        assert_eq!(transfers_that_fit(RW::R, 1024), 255);
    }

    #[test]
    fn a_packet_stops_at_the_transfer_count_field() {
        // The largest packets the count field can describe in full: 1282 bytes of writes, 1026 of
        // reads. Past those the packet has room the byte cannot express.
        assert_eq!(transfers_that_fit(RW::W, 1282), 255);
        assert_eq!(transfers_that_fit(RW::W, 1283), 255);
        assert_eq!(transfers_that_fit(RW::R, 1026), 255);
        assert_eq!(transfers_that_fit(RW::R, 1027), 255);
        assert_eq!(transfers_that_fit(RW::R, u16::MAX), 255);
    }

    #[test]
    fn a_scattered_run_fills_a_packet_up_to_the_count_field() {
        // An address write then a data read, per address. Charged the write price throughout, a
        // 1024-byte packet took 204 transfers, 102 addresses. Priced per direction the count field
        // binds first: 255 transfers, 127 addresses read in full and one whose data read falls
        // into the next packet.
        let mut ops = Vec::new();
        for _ in 0..200 {
            ops.push(transfer(Port::Ap, 0b0100, Direction::Write, 0x2000_0000));
            ops.push(transfer(Port::Ap, 0b1100, Direction::Read, 0));
        }

        let packets: Vec<_> = split_into_packets(&ops, 0, 0, 1024).collect();
        assert_eq!(packets[0].transfers.len(), 255);
    }

    #[test]
    fn batch_splits_at_packet_limit_without_read_flush() {
        let packet_size = 64u16;
        let max_per_packet = transfers_that_fit(RW::R, packet_size);
        let mut ops = Vec::new();
        for index in 0..(max_per_packet + 2) {
            // Alternating addresses keep the run below MIN_BLOCK_TRANSFERS, so
            // every transfer stays in DAP_Transfer.
            let addr = if index % 2 == 0 { 0b0100 } else { 0b1000 };
            ops.push(transfer(Port::Dp, addr, Direction::Read, 0));
        }

        let packets: Vec<_> = split_into_packets(&ops, 0, 0, packet_size).collect();
        assert_eq!(packets.len(), 2);
        assert_eq!(packets[0].transfers.len(), max_per_packet);
        assert_eq!(packets[1].transfers.len(), 2);
    }

    #[test]
    fn handles_wait_follows_transfer_configure() {
        assert!(!cmsis_handles_wait(0));
        assert!(cmsis_handles_wait(1));
        assert!(cmsis_handles_wait(0xffff));
    }

    fn transfer(port: Port, addr: u8, direction: Direction, data: u32) -> (HandleId, SwdOp) {
        (
            HandleId::new(),
            SwdOp::Transfer {
                port,
                addr,
                direction,
                data,
            },
        )
    }

    /// One item for [`PendingBatch::push_all`].
    fn item(
        id: &HandleId,
        addr: u8,
        data: u32,
        batch_index: usize,
    ) -> (HandleId, RegisterAddress, u32, usize) {
        (
            id.clone(),
            RegisterAddress::ApRegister(addr),
            data,
            batch_index,
        )
    }

    #[test]
    fn an_empty_packet_takes_one_transfer_however_small() {
        let mut packet = PendingBatch::new(0);
        let (id, _) = transfer(Port::Ap, 0b0100, Direction::Write, 0);

        // An empty packet takes its first transfer whatever it costs, or splitting a batch would
        // not make progress.
        assert!(packet.push_all(Direction::Write, 8, [item(&id, 0b0100, 0x2000_0000, 0)]));

        assert!(!packet.push_all(Direction::Write, 8, [item(&id, 0b0100, 0, 1)]));
        assert!(packet.push_all(Direction::Read, 64, [item(&id, 0b1100, 0, 2)]));
    }

    /// An address write followed by `reads` data reads, the shape of a block read.
    fn address_then_reads(reads: usize) -> Vec<(HandleId, SwdOp)> {
        let mut ops = vec![transfer(Port::Ap, 0b0100, Direction::Write, 0x2000_0000)];
        ops.extend((0..reads).map(|_| transfer(Port::Ap, 0b1100, Direction::Read, 0)));
        ops
    }

    #[test]
    fn a_repeat_that_fits_travels_with_the_address_that_set_it_up() {
        let packets: Vec<_> = split_into_packets(&address_then_reads(2), 0, 0, 64).collect();

        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].transfers.len(), 3);
    }

    #[test]
    fn a_repeat_rides_along_until_the_reply_is_full() {
        // Three header bytes and four per read leave room for fifteen in a 64-byte reply, and the
        // address write costs the reply nothing.
        let fits: Vec<_> = split_into_packets(&address_then_reads(15), 0, 0, 64).collect();
        assert_eq!(fits.len(), 1);
        assert_eq!(fits[0].transfers.len(), 16);

        let one_more: Vec<_> = split_into_packets(&address_then_reads(16), 0, 0, 64).collect();
        assert_eq!(one_more.len(), 1);
        assert_eq!(one_more[0].transfers.len(), 1);
    }

    #[test]
    fn a_repeat_too_long_for_the_packet_is_left_to_the_block_command() {
        let packets: Vec<_> = split_into_packets(&address_then_reads(40), 0, 0, 64).collect();

        // Only the address write is packed. `run_batch` sends the reads as `DAP_TransferBlock`,
        // which is pipelined.
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].transfers.len(), 1);
    }

    #[test]
    fn a_run_that_does_not_fit_leaves_the_packet_as_it_was() {
        let mut packet = PendingBatch::new(0);
        let (id, _) = transfer(Port::Ap, 0b0100, Direction::Write, 0);

        assert!(packet.push_all(Direction::Write, 64, [item(&id, 0b0100, 0x2000_0000, 0)]));
        let (request, response) = packet.request.packet_lengths();

        // Long enough to run out of room part way.
        let run = (0..40).map(|i| item(&id, 0b1100, 0, i + 1));
        assert!(!packet.push_all(Direction::Read, 64, run));

        assert_eq!(packet.transfers.len(), 1);
        assert_eq!(packet.request.packet_lengths(), (request, response));
    }

    #[test]
    fn run_length_counts_the_same_access_only() {
        let ops = vec![
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
            transfer(Port::Ap, 0b1000, Direction::Read, 0),
        ];

        assert_eq!(run_length(&ops, 0), 3);
        assert_eq!(run_length(&ops, 3), 1);
    }

    #[test]
    fn run_length_stops_at_a_write_and_at_an_idle() {
        let ops = vec![
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
            transfer(Port::Ap, 0b1100, Direction::Write, 0),
        ];
        assert_eq!(run_length(&ops, 0), 1);

        let ops = vec![
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
            (HandleId::new(), SwdOp::Idle { cycles: 8 }),
            transfer(Port::Ap, 0b1100, Direction::Read, 0),
        ];
        assert_eq!(run_length(&ops, 0), 1);
    }

    #[test]
    fn a_block_packet_holds_more_written_words_than_a_transfer_packet() {
        // A block sends the access once for the whole run, so its command has the room the
        // per-transfer path spends on a request byte per word.
        for packet_size in [64u16, 512, 1024] {
            assert!(block_words_per_packet(packet_size) > transfers_that_fit(RW::W, packet_size));
        }
    }

    #[test]
    fn block_read_request_encodes_the_access_once() {
        let mut request =
            TransferBlockRequest::read_request(RegisterAddress::ApRegister(0b1100), 3);
        request.dap_index = 1;

        let mut buffer = [0u8; 16];
        let size = request.to_bytes(&mut buffer).unwrap();

        assert_eq!(size, 4);
        assert_eq!(buffer[0], 1);
        assert_eq!(u16::from_le_bytes([buffer[1], buffer[2]]), 3);
        // AP, read, A2 and A3 set.
        assert_eq!(buffer[3], 0b1111);
    }

    #[test]
    fn block_write_request_appends_the_data_words() {
        let request = TransferBlockRequest::write_request(
            RegisterAddress::ApRegister(0b1100),
            vec![0x1111_2222, 0x3333_4444],
        );

        let mut buffer = [0u8; 16];
        let size = request.to_bytes(&mut buffer).unwrap();

        assert_eq!(size, 4 + 2 * 4);
        assert_eq!(u16::from_le_bytes([buffer[1], buffer[2]]), 2);
        // AP, write, A2 and A3 set.
        assert_eq!(buffer[3], 0b1101);
        assert_eq!(
            u32::from_le_bytes(buffer[4..8].try_into().unwrap()),
            0x1111_2222
        );
        assert_eq!(
            u32::from_le_bytes(buffer[8..12].try_into().unwrap()),
            0x3333_4444
        );
    }

    fn pending_transfers(n: usize) -> Vec<PendingTransfer> {
        (0..n)
            .map(|i| PendingTransfer {
                id: HandleId::new(),
                direction: Direction::Read,
                batch_index: 100 + i,
            })
            .collect()
    }

    fn transfer_response(count: usize, ack: Ack) -> TransferResponse {
        TransferResponse {
            last_transfer_response: LastTransferResponse {
                ack,
                protocol_error: false,
                _value_mismatch: false,
            },
            transfers: (0..count)
                .map(
                    |_| crate::probe::cmsisdap::commands::transfer::InnerTransferResponse {
                        td_timestamp: None,
                        data: Some(0),
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn a_faulting_transfer_is_reported_as_a_fault_not_as_a_short_count() {
        // The probe stops early *because* of the FAULT, so the short count must not mask it.
        // Reporting a bare probe error here skips sticky-error recovery and leaves
        // CTRL/STAT.STICKYERR set, after which every later AP access faults (IHI0031G B4.2.4).
        let transfers = pending_transfers(4);
        let response = transfer_response(2, Ack::Fault);

        let (error, fault_operation) =
            CmsisDap::classify_transfer_response(&response, &transfers).unwrap_err();

        assert!(
            matches!(
                error,
                BatchError::Specific(DebugProbeError::SwdTransfer(
                    SwdTransferError::FaultResponse
                ))
            ),
            "got {error:?}"
        );
        assert_eq!(fault_operation, 101);
    }

    #[test]
    fn a_waiting_transfer_is_reported_as_a_wait_not_as_a_short_count() {
        let transfers = pending_transfers(4);
        let response = transfer_response(1, Ack::Wait);

        let (error, _) = CmsisDap::classify_transfer_response(&response, &transfers).unwrap_err();

        assert!(
            matches!(
                error,
                BatchError::Specific(DebugProbeError::SwdTransfer(SwdTransferError::WaitResponse))
            ),
            "got {error:?}"
        );
    }

    #[test]
    fn a_short_transfer_count_with_an_ok_ack_is_reported_against_the_probe() {
        let transfers = pending_transfers(4);
        let response = transfer_response(3, Ack::Ok);

        let (error, fault_operation) =
            CmsisDap::classify_transfer_response(&response, &transfers).unwrap_err();

        assert!(matches!(
            error,
            BatchError::Probe(DebugProbeError::Other(_))
        ));
        assert_eq!(fault_operation, 102);
    }

    #[test]
    fn a_complete_transfer_reply_is_a_success() {
        let transfers = pending_transfers(4);
        let response = transfer_response(4, Ack::Ok);

        assert!(CmsisDap::classify_transfer_response(&response, &transfers).is_ok());
    }
}
