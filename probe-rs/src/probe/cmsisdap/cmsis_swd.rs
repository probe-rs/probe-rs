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
                transfer::{Ack, RW, TransferBlockRequest, TransferBlockResponse, TransferRequest},
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

/// How many `DAP_TransferBlock` requests may be outstanding at once.
///
/// Overlapping a request with the previous reply hides one round trip each time. Past a handful
/// the wait is already covered and a deeper queue only holds more requests in memory.
const MAX_PIPELINED_BLOCKS: usize = 8;

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

/// How far a run of transfers that `DAP_Transfer` carries extends from `start`.
///
/// It ends at anything that is not a transfer, and at a repeat long enough for
/// `DAP_TransferBlock`.
fn transfer_run_end(ops: &[(HandleId, SwdOp)], start: usize) -> usize {
    let mut end = start;
    while end < ops.len()
        && matches!(ops[end].1, SwdOp::Transfer { .. })
        && run_length(ops, end) < MIN_BLOCK_TRANSFERS
    {
        end += 1;
    }
    end
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
        // One request per packet, so there is no carried state to reset between them.
        let mut packet = PendingBatch::new(dap_index);

        while let Some((id, op)) = ops.get(offset) {
            let SwdOp::Transfer {
                port,
                addr,
                direction,
                data,
            } = op
            else {
                break;
            };

            // An empty command always takes one more transfer, so this terminates however small
            // the packet is.
            if !packet.is_empty() && !packet.has_room_for(*direction, packet_size) {
                break;
            }

            let address = CmsisDap::swd_register(*port, *addr);
            packet.push(id.clone(), address, *direction, *data, run_start + offset);
            offset += 1;
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
/// is what lets a reply be matched to the batch operation that asked for it. Only [`Self::push`]
/// adds to either.
struct PendingBatch {
    request: TransferRequest,
    transfers: Vec<PendingTransfer>,
}

impl PendingBatch {
    fn new(dap_index: u8) -> Self {
        let mut request = TransferRequest::empty();
        request.dap_index = dap_index;
        Self {
            request,
            transfers: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    /// True when a `packet_size` packet still has room for one more transfer in `direction`.
    fn has_room_for(&self, direction: Direction, packet_size: u16) -> bool {
        let rw = match direction {
            Direction::Read => RW::R,
            Direction::Write => RW::W,
        };
        self.request.has_room_for(rw, packet_size)
    }

    fn push(
        &mut self,
        id: HandleId,
        address: RegisterAddress,
        direction: Direction,
        data: u32,
        batch_index: usize,
    ) {
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

    fn flush_pending_transfers(
        &mut self,
        pending: &PendingBatch,
        mut results: Results,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        if pending.is_empty() {
            return Ok(results);
        }

        let transfers = &pending.transfers;
        let response = match commands::send_command(&mut self.device, &pending.request) {
            Ok(response) => response,
            Err(error) => {
                let fault_operation = transfers[0].batch_index;
                return Err(BatchExecutionError::new_from_debug_probe_at(
                    DebugProbeError::from(error),
                    results,
                    fault_operation,
                ));
            }
        };

        let count = response.transfers.len();
        if response.last_transfer_response.protocol_error {
            let fault_operation = transfers[count.saturating_sub(1)].batch_index;
            return Err(BatchExecutionError {
                error: BatchError::Specific(DebugProbeError::SwdTransfer(
                    SwdTransferError::Protocol,
                )),
                results,
                fault_operation,
            });
        }

        if count < transfers.len() {
            let fault_operation = transfers[count.saturating_sub(1)].batch_index;
            return Err(BatchExecutionError::new_from_debug_probe_at(
                DebugProbeError::Other(format!(
                    "Possible error in CMSIS-DAP probe: Only {}/{} transfers were executed, but no error was reported.",
                    count,
                    transfers.len()
                )),
                results,
                fault_operation,
            ));
        }

        match response.last_transfer_response.ack {
            Ack::Ok => {
                for (transfer, response_transfer) in transfers.iter().zip(response.transfers.iter())
                {
                    if transfer.direction != Direction::Read || !transfer.id.should_capture() {
                        continue;
                    }
                    let Some(data) = response_transfer.data else {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            DebugProbeError::Other(
                                "CMSIS-DAP read did not return any data".to_string(),
                            ),
                            results,
                            transfer.batch_index,
                        ));
                    };
                    results.push(&transfer.id, CommandResult::U32(data));
                }
                Ok(results)
            }
            Ack::Fault => {
                let fault_operation = transfers[count.saturating_sub(1)].batch_index;
                if let Err(error) = self.handle_sticky_err() {
                    tracing::warn!("Failed to clear the sticky error: {error}");
                }
                Err(BatchExecutionError {
                    error: BatchError::Specific(DebugProbeError::SwdTransfer(
                        SwdTransferError::FaultResponse,
                    )),
                    results,
                    fault_operation,
                })
            }
            Ack::Wait => {
                let fault_operation = transfers[count.saturating_sub(1)].batch_index;
                let abort = {
                    let mut abort = Abort(0);
                    abort.set_dapabort(true);
                    abort
                };
                if let Err(error) = self.write_abort(abort) {
                    tracing::warn!("Failed to abort the transfer: {error}");
                }
                Err(BatchExecutionError {
                    error: BatchError::Specific(DebugProbeError::SwdTransfer(
                        SwdTransferError::WaitResponse,
                    )),
                    results,
                    fault_operation,
                })
            }
            ack => {
                let fault_operation = transfers[count.saturating_sub(1)].batch_index;
                Err(BatchExecutionError {
                    error: BatchError::Specific(DebugProbeError::SwdTransfer(
                        Self::ack_to_transfer_error(ack),
                    )),
                    results,
                    fault_operation,
                })
            }
        }
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

    /// Send a repeated access as `DAP_TransferBlock` packets, several at a time.
    ///
    /// Sending the next request before reading the previous reply hides one round trip per
    /// overlap. ADIv5 §B4.2.4 describes this as the intended use: "A debugger might stream a block
    /// of data and then check the CTRL/STAT register at the end of the block."
    ///
    /// A request already on the wire when an earlier one faults cannot reach the target. Once a
    /// sticky flag is set the DP answers FAULT to every access except reads of DPIDR and
    /// CTRL/STAT and writes to ABORT, so the tail is refused rather than performed, and the first
    /// failure in batch order is the one reported.
    fn run_transfer_block(
        &mut self,
        run: &[(HandleId, SwdOp)],
        run_start: usize,
        mut results: Results,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let SwdOp::Transfer {
            port,
            addr,
            direction,
            ..
        } = run[0].1
        else {
            return Ok(results);
        };
        let address = Self::swd_register(port, addr);
        let words_per_packet = self.max_words_per_block_packet();
        let depth = (self.packet_count as usize).clamp(1, MAX_PIPELINED_BLOCKS);

        let mut chunks = run.chunks(words_per_packet).enumerate();
        let mut in_flight = VecDeque::with_capacity(depth);
        let mut failure: Option<(BatchError<DebugProbeError>, usize)> = None;
        // A chunk that failed ends the run, so nothing after it is the caller's to see. A chunk
        // that was never sent does not: whatever is already in flight ran before the failure and
        // would have run without the pipeline too.
        let mut capture = true;

        loop {
            while failure.is_none() && in_flight.len() < depth {
                let Some((chunk_index, chunk)) = chunks.next() else {
                    break;
                };
                let chunk_start = run_start + chunk_index * words_per_packet;

                let mut request = match direction {
                    Direction::Read => {
                        TransferBlockRequest::read_request(address, chunk.len() as u16)
                    }
                    Direction::Write => TransferBlockRequest::write_request(
                        address,
                        chunk.iter().map(|(_, op)| transfer_data(op)).collect(),
                    ),
                };
                request.dap_index = self.jtag_state.chain_params.index as u8;

                if let Err(error) = commands::send_request(&mut self.device, &request) {
                    failure = Some((BatchError::Probe(DebugProbeError::from(error)), chunk_start));
                    break;
                }

                in_flight.push_back((chunk_start, chunk, request));
            }

            let Some((chunk_start, chunk, request)) = in_flight.pop_front() else {
                break;
            };

            let response = match commands::receive_response(&mut self.device, &request) {
                Ok(response) => response,
                Err(error) => {
                    failure.get_or_insert((
                        BatchError::Probe(DebugProbeError::from(error)),
                        chunk_start,
                    ));
                    capture = false;
                    continue;
                }
            };

            match Self::classify_block_response(&response, chunk.len(), chunk_start) {
                Err(problem) => {
                    failure.get_or_insert(problem);
                    capture = false;
                }
                Ok(()) if !capture => {}
                Ok(()) => {
                    if direction == Direction::Read {
                        for ((id, _), value) in chunk.iter().zip(response.transfer_data.iter()) {
                            if id.should_capture() {
                                results.push(id, CommandResult::U32(*value));
                            }
                        }
                    }
                }
            }
        }

        match failure {
            Some((error, fault_operation)) => {
                self.recover_from_block_error(&error);
                Err(BatchExecutionError {
                    error,
                    results,
                    fault_operation,
                })
            }
            None => Ok(results),
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
    ) -> Result<(), DebugProbeError> {
        self.connect_if_needed()?;

        let request = commands::swj::pins::SWJPinsRequest::from_raw_values(
            out.0,
            select.0,
            wait.as_micros() as u32,
        );
        commands::send_command(&mut self.device, &request)?;
        Ok(())
    }
}

impl SwdProbe for CmsisDap {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        let mut results = Results::new();
        let ops: Vec<(HandleId, SwdOp)> = batch
            .iter()
            .map(|(id, op)| (id.clone(), op.clone()))
            .collect();
        let dap_index = self.jtag_state.chain_params.index as u8;

        let mut batch_index = 0;
        while batch_index < ops.len() {
            let run_end = transfer_run_end(&ops, batch_index);
            let packets = split_into_packets(
                &ops[batch_index..run_end],
                batch_index,
                dap_index,
                self.packet_size,
            );
            for packet in packets {
                results = self.flush_pending_transfers(&packet, results)?;
            }
            batch_index = run_end;

            // Whatever ended the run is handled on its own, and cannot share a packet.
            let Some((_, op)) = ops.get(batch_index) else {
                break;
            };

            match op {
                SwdOp::Transfer { .. } => {
                    let run = run_length(&ops, batch_index);
                    results = self.run_transfer_block(
                        &ops[batch_index..batch_index + run],
                        batch_index,
                        results,
                    )?;
                    batch_index += run;
                    continue;
                }
                SwdOp::Sequence(bits) => {
                    if let Err(error) = self.run_swj_sequence_op(bits) {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            error,
                            results,
                            batch_index,
                        ));
                    }
                }
                SwdOp::Idle { cycles } => {
                    if let Err(error) = self.run_swj_idle(*cycles) {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            error,
                            results,
                            batch_index,
                        ));
                    }
                }
                SwdOp::Pins { out, select, wait } => {
                    if let Err(error) = self.run_swj_pins_op(*out, *select, *wait) {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            error,
                            results,
                            batch_index,
                        ));
                    }
                }
            }

            batch_index += 1;
        }

        Ok(results)
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
        while request.has_room_for(rw, packet_size) {
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
}
