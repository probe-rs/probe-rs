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
                transfer::{Ack, TransferRequest},
            },
        },
        swd::{Direction, Pins as SwdPins, Port, SwdBatch, SwdOp, SwdProbe, SwdTransferError},
    },
};

pub(crate) fn cmsis_handles_wait(wait_retry: u16) -> bool {
    wait_retry > 0
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
                    if transfer.direction == Direction::Read && transfer.id.should_capture() {
                        results.push(
                            &transfer.id,
                            CommandResult::U32(
                                response_transfer.data.expect(
                                    "CMSIS-DAP probe should always return data for a read.",
                                ),
                            ),
                        );
                    }
                }
                Ok(results)
            }
            Ack::Fault => {
                let fault_operation = pending[count.saturating_sub(1)].batch_index;
                let _ = self.handle_sticky_err();
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
                let _ = self.write_abort({
                    let mut abort = Abort(0);
                    abort.set_dapabort(true);
                    abort
                });
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

        for (batch_index, (id, op)) in batch.iter().enumerate() {
            match op {
                SwdOp::Transfer {
                    port,
                    addr,
                    direction,
                    data,
                } => {
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
        }

        self.flush_pending_transfers(&pending, results)
    }

    fn handles_wait(&self) -> bool {
        cmsis_handles_wait(self.transfer_wait_retry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::swd::{Direction, Port, SwdOp};

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
        for _ in 0..(max_per_packet + 2) {
            ops.push(SwdOp::Transfer {
                port: Port::Dp,
                addr: 0b0100,
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
}
