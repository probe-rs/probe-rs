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
                transfer::{Ack, TransferBlockRequest, TransferBlockResponse, TransferRequest},
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

struct PendingTransfer {
    id: HandleId,
    port: Port,
    addr: u8,
    direction: Direction,
    data: u32,
    batch_index: usize,
}

impl CmsisDap {
    pub(crate) fn max_transfers_per_packet(&self) -> usize {
        (self.packet_size as usize - 3) / (1 + 4)
    }

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
        pending: &[PendingTransfer],
        mut results: Results,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        if pending.is_empty() {
            return Ok(results);
        }

        let mut request = TransferRequest::empty();
        request.dap_index = self.jtag_state.chain_params.index as u8;
        for transfer in pending {
            let address = Self::swd_register(transfer.port, transfer.addr);
            match transfer.direction {
                Direction::Read => request.add_read(address),
                Direction::Write => request.add_write(address, transfer.data),
            }
        }

        let response = match commands::send_command(&mut self.device, &request) {
            Ok(response) => response,
            Err(error) => {
                let fault_operation = pending[0].batch_index;
                return Err(BatchExecutionError::new_from_debug_probe_at(
                    DebugProbeError::from(error),
                    results,
                    fault_operation,
                ));
            }
        };

        let count = response.transfers.len();
        if response.last_transfer_response.protocol_error {
            let fault_operation = pending[count.saturating_sub(1)].batch_index;
            return Err(BatchExecutionError {
                error: BatchError::Specific(DebugProbeError::SwdTransfer(
                    SwdTransferError::Protocol,
                )),
                results,
                fault_operation,
            });
        }

        if count < pending.len() {
            let fault_operation = pending[count.saturating_sub(1)].batch_index;
            return Err(BatchExecutionError::new_from_debug_probe_at(
                DebugProbeError::Other(format!(
                    "Possible error in CMSIS-DAP probe: Only {}/{} transfers were executed, but no error was reported.",
                    count,
                    pending.len()
                )),
                results,
                fault_operation,
            ));
        }

        match response.last_transfer_response.ack {
            Ack::Ok => {
                for (transfer, response_transfer) in pending.iter().zip(response.transfers.iter()) {
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
                let fault_operation = pending[count.saturating_sub(1)].batch_index;
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
                let fault_operation = pending[count.saturating_sub(1)].batch_index;
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
                let fault_operation = pending[count.saturating_sub(1)].batch_index;
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
        let mut pending = Vec::new();
        let max_per_packet = self.max_transfers_per_packet();
        let ops: Vec<(HandleId, SwdOp)> = batch
            .iter()
            .map(|(id, op)| (id.clone(), op.clone()))
            .collect();

        let mut batch_index = 0;
        while batch_index < ops.len() {
            let (id, op) = &ops[batch_index];
            match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
                    let run = run_length(&ops, batch_index);
                    if run >= MIN_BLOCK_TRANSFERS {
                        results = self.flush_pending_transfers(&pending, results)?;
                        pending.clear();
                        results = self.run_transfer_block(
                            &ops[batch_index..batch_index + run],
                            batch_index,
                            results,
                        )?;
                        batch_index += run;
                        continue;
                    }

                    pending.push(PendingTransfer {
                        id: id.clone(),
                        port: *port,
                        addr: *addr,
                        direction: *direction,
                        data: *data,
                        batch_index,
                    });

                    if pending.len() >= max_per_packet {
                        results = self.flush_pending_transfers(&pending, results)?;
                        pending.clear();
                    }
                }
                SwdOp::Sequence(bits) => {
                    results = self.flush_pending_transfers(&pending, results)?;
                    pending.clear();
                    if let Err(error) = self.run_swj_sequence_op(bits) {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            error,
                            results,
                            batch_index,
                        ));
                    }
                }
                SwdOp::Idle { cycles } => {
                    results = self.flush_pending_transfers(&pending, results)?;
                    pending.clear();
                    if let Err(error) = self.run_swj_idle(*cycles) {
                        return Err(BatchExecutionError::new_from_debug_probe_at(
                            error,
                            results,
                            batch_index,
                        ));
                    }
                }
                SwdOp::Pins { out, select, wait } => {
                    results = self.flush_pending_transfers(&pending, results)?;
                    pending.clear();
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

        self.flush_pending_transfers(&pending, results)
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

    #[derive(Clone)]
    struct RecordedTransfer {
        port: Port,
        direction: Direction,
        data: Option<u32>,
    }

    fn encode_transfer_ops(ops: &[SwdOp]) -> Vec<RecordedTransfer> {
        ops.iter()
            .filter_map(|op| match op {
                SwdOp::Transfer {
                    port,
                    direction,
                    data,
                    ..
                } => Some(RecordedTransfer {
                    port: *port,
                    direction: *direction,
                    data: (*direction == Direction::Write).then_some(*data),
                }),
                _ => None,
            })
            .collect()
    }

    fn split_transfer_ops(ops: &[SwdOp], packet_size: u16) -> Vec<Vec<RecordedTransfer>> {
        let max_per_packet = (packet_size as usize - 3) / 5;
        let transfers = encode_transfer_ops(ops);
        let mut chunks = Vec::new();
        let mut offset = 0;
        while offset < transfers.len() {
            let end = (offset + max_per_packet).min(transfers.len());
            chunks.push(transfers[offset..end].to_vec());
            offset = end;
        }
        chunks
    }

    #[test]
    fn transfer_encoder_matches_swd_ops() {
        let ops = [
            SwdOp::Transfer {
                port: Port::Dp,
                addr: 0b0100,
                direction: Direction::Read,
                data: 0,
            },
            SwdOp::Transfer {
                port: Port::Ap,
                addr: 0b1000,
                direction: Direction::Write,
                data: 0x1234_5678,
            },
        ];

        let encoded = encode_transfer_ops(&ops);
        assert_eq!(encoded.len(), 2);
        assert_eq!(encoded[0].port, Port::Dp);
        assert_eq!(encoded[0].direction, Direction::Read);
        assert_eq!(encoded[1].data, Some(0x1234_5678));
    }

    #[test]
    fn batch_splits_at_packet_limit_without_read_flush() {
        let packet_size = 64u16;
        let max_per_packet = (packet_size as usize - 3) / 5;
        let mut ops = Vec::new();
        for index in 0..(max_per_packet + 2) {
            // Alternating addresses keep the run below MIN_BLOCK_TRANSFERS, so
            // every transfer stays in DAP_Transfer.
            ops.push(SwdOp::Transfer {
                port: Port::Dp,
                addr: if index % 2 == 0 { 0b0100 } else { 0b1000 },
                direction: Direction::Read,
                data: 0,
            });
        }

        let chunks = split_transfer_ops(&ops, packet_size);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), max_per_packet);
        assert_eq!(chunks[1].len(), 2);
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
    fn a_block_packet_holds_more_words_than_a_transfer_packet() {
        for packet_size in [64u16, 512, 1024] {
            let per_transfer_packet = (packet_size as usize - 3) / 5;
            assert!(block_words_per_packet(packet_size) > per_transfer_packet);
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
