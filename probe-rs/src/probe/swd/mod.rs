//! Layer 0 SWD operations for probe drivers.
//!
//! The ADIv5 layer above consumes these operations.
//!
//! # Example
//!
//! ```
//! use probe_rs::probe::{BitSequence, Port, SwdBatch};
//!
//! let mut batch = SwdBatch::new();
//! let _value = batch.read(Port::Dp, 0b0100);
//! batch.write(Port::Ap, 0b1000, 0x1234_5678);
//! batch.sequence(BitSequence::from_u64(16, 0xE24E));
//! batch.idle(8);
//! ```

/// One step of a [`BitbangSwd::swd_io`] sequence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum IoSequenceItem {
    /// Drive SWDIO to the given level for one clock.
    Output(bool),
    /// Sample SWDIO for one clock.
    Input,
}

/// SWD wire-protocol timing settings used by [`BitbangSwd`] probes.
#[derive(Debug, Clone)]
pub struct SwdSettings {
    /// Initial number of idle cycles between consecutive writes.
    ///
    /// When a WAIT response is received, the number of idle cycles
    /// will be increased automatically, so this number can be quite
    /// low.
    pub num_idle_cycles_between_writes: usize,

    /// How often a SWD transfer is retried when a WAIT response
    /// is received.
    pub num_retries_after_wait: usize,

    /// When a SWD transfer is retried due to a WAIT response, the idle
    /// cycle amount is doubled every time as a backoff. This sets a maximum
    /// cap to the cycle amount.
    pub max_retry_idle_cycles_after_wait: usize,

    /// Number of idle cycles inserted before the result
    /// of a write is checked.
    pub idle_cycles_before_write_verify: usize,

    /// Number of idle cycles to insert after a transfer.
    pub idle_cycles_after_transfer: usize,
}

impl Default for SwdSettings {
    fn default() -> Self {
        Self {
            num_idle_cycles_between_writes: 2,
            num_retries_after_wait: 1000,
            max_retry_idle_cycles_after_wait: 128,
            idle_cycles_before_write_verify: 8,
            idle_cycles_after_transfer: 8,
        }
    }
}

mod port;

#[cfg(test)]
pub(crate) mod mock;

use std::time::Duration;

use crate::probe::{
    Batch, BatchError, BatchExecutionError, BitSequence, CommandResult, DebugProbe,
    DebugProbeError, Handle, HandleId, Results,
};

pub use port::{SwdPort, SwdPortError};

bitfield::bitfield! {
    /// A struct to describe the default CMSIS-DAP pins that one can toggle from the host.
    #[derive(Copy, Clone)]
    pub struct Pins(u8);
    impl Debug;
    /// The active low reset of the debug probe.
    pub nreset, set_nreset: 7;
    /// The negative target reset pin of JTAG.
    pub ntrst, set_ntrst: 5;
    /// The TDO or SWO pin.
    pub tdo, set_tdo: 3;
    /// The TDI pin.
    pub tdi, set_tdi: 2;
    /// The SWDIO or TMS pin.
    pub swdio_tms, set_swdio_tms: 1;
    /// The clock pin.
    pub swclk_tck, set_swclk_tck: 0;
}

const A2_MASK: u8 = 0b0100;
const A3_MASK: u8 = 0b1000;

const REQUEST_BITS: usize = 8;
const TRANSFER_RESPONSE_BITS: usize = REQUEST_BITS + 3 + 32 + 1 + 2;

fn a2(addr: u8) -> bool {
    (addr & A2_MASK) == A2_MASK
}

fn a3(addr: u8) -> bool {
    (addr & A3_MASK) == A3_MASK
}

/// The debug port that a transfer targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Port {
    /// The debug port.
    Dp,
    /// An access port.
    Ap,
}

/// The direction of a register transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Read a register.
    Read,
    /// Write a register.
    Write,
}

/// One SWD operation in a batch.
#[derive(Clone, Debug)]
pub enum SwdOp {
    /// Transfer one ADIv5 register.
    Transfer {
        /// The debug port or access port.
        port: Port,
        /// Bits 2 and 3 of the register address.
        addr: u8,
        /// The transfer direction.
        direction: Direction,
        /// The write data. A read passes zero.
        data: u32,
    },
    /// Drive SWDIO as an output.
    Sequence(BitSequence),
    /// Drive SWDIO low for the given number of clock cycles.
    Idle {
        /// The number of idle cycles.
        cycles: u32,
    },
    /// Drive the CMSIS-DAP SWJ pins.
    Pins {
        /// The output levels.
        out: Pins,
        /// The pin select mask.
        select: Pins,
        /// The maximum wait time.
        wait: Duration,
    },
}

/// A batch of SWD operations.
pub type SwdBatch = Batch<SwdOp, DebugProbeError>;

impl SwdBatch {
    /// Schedule a read and return a handle for the captured value.
    pub fn read(&mut self, port: Port, addr: u8) -> Handle<u32> {
        self.schedule(SwdOp::Transfer {
            port,
            addr,
            direction: Direction::Read,
            data: 0,
        })
        .map(|result| match result {
            CommandResult::U32(value) => value,
            _ => panic!("unexpected CommandResult variant for an SWD read"),
        })
    }

    /// Schedule a write.
    pub fn write(&mut self, port: Port, addr: u8, data: u32) {
        let _ = self.schedule(SwdOp::Transfer {
            port,
            addr,
            direction: Direction::Write,
            data,
        });
    }

    /// Schedule an output-only bit sequence.
    pub fn sequence(&mut self, bits: BitSequence) {
        let _ = self.schedule(SwdOp::Sequence(bits));
    }

    /// Schedule idle clock cycles with SWDIO driven low.
    pub fn idle(&mut self, cycles: u32) {
        let _ = self.schedule(SwdOp::Idle { cycles });
    }
}

/// An error response from an SWD transfer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error, docsplay::Display)]
pub enum SwdTransferError {
    /// Target device did not respond to request.
    NoAcknowledge,
    /// Target device responded with a FAULT response to the request.
    FaultResponse,
    /// Target device responded with a WAIT response to the request.
    WaitResponse,
    /// The parity bit on the read request was incorrect.
    IncorrectParity,
    /// A protocol error occurred during SWD communication.
    Protocol,
}

/// Build the wire sequence of one SWD transfer.
pub(crate) fn transfer_io_sequence(
    port: Port,
    addr: u8,
    direction: Direction,
    data: u32,
) -> Vec<IoSequenceItem> {
    let ap_n_dp = port == Port::Ap;
    let direction_bit = direction == Direction::Read;
    let a2_bit = a2(addr);
    let a3_bit = a3(addr);

    let mut sequence = Vec::with_capacity(TRANSFER_RESPONSE_BITS);

    sequence.push(IoSequenceItem::Output(true));
    sequence.push(IoSequenceItem::Output(ap_n_dp));
    sequence.push(IoSequenceItem::Output(direction_bit));
    sequence.push(IoSequenceItem::Output(a2_bit));
    sequence.push(IoSequenceItem::Output(a3_bit));
    sequence.push(IoSequenceItem::Output(
        ap_n_dp ^ direction_bit ^ a2_bit ^ a3_bit,
    ));
    sequence.push(IoSequenceItem::Output(false));
    sequence.push(IoSequenceItem::Output(true));
    sequence.push(IoSequenceItem::Input);

    for _ in 0..3 {
        sequence.push(IoSequenceItem::Input);
    }

    if direction == Direction::Write {
        sequence.push(IoSequenceItem::Input);

        for i in 0..32 {
            sequence.push(IoSequenceItem::Output(data & (1 << i) != 0));
        }

        sequence.push(IoSequenceItem::Output(data.count_ones() % 2 == 1));
    } else {
        for _ in 0..32 {
            sequence.push(IoSequenceItem::Input);
        }

        sequence.push(IoSequenceItem::Input);
        sequence.push(IoSequenceItem::Input);
    }

    sequence
}

/// Parse the sampled bits of an SWD transfer.
pub(crate) fn parse_transfer_response(
    resp: &[bool],
    direction: Direction,
) -> Result<u32, SwdTransferError> {
    let (ack, response) = resp.split_at(3);

    match (ack[0], ack[1], ack[2]) {
        (true, true, true) => Err(SwdTransferError::NoAcknowledge),
        (false, true, false) => Err(SwdTransferError::WaitResponse),
        (false, false, true) => Err(SwdTransferError::FaultResponse),
        (true, false, false) if direction == Direction::Read => {
            let value = crate::probe::common::bits_to_byte(response.iter().copied());

            if value.count_ones() % 2 == response[32] as u32 {
                Ok(value)
            } else {
                Err(SwdTransferError::IncorrectParity)
            }
        }
        (true, false, false) => Ok(0),
        _ => Err(SwdTransferError::Protocol),
    }
}

/// A probe that executes [`SwdBatch`] values.
///
/// The implementation runs the operations in order. It reorders nothing.
///
/// The implementation may skip the capture of a read whose handle the caller
/// dropped. `HandleId::should_capture` reports this.
///
/// The implementation returns a WAIT and a FAULT to the caller. It does not
/// retry. `SwdPort` retries. A probe whose firmware retries says so with
/// [`SwdProbe::handles_wait`].
///
/// On a fault, the implementation returns the results before the fault, and
/// the index of the operation that failed.
pub trait SwdProbe: DebugProbe {
    /// Execute a batch of SWD operations.
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>>;

    /// Report whether the probe owns WAIT and FAULT completely.
    ///
    /// When this returns true, the host does not retry WAIT responses.
    fn handles_wait(&self) -> bool {
        false
    }

    /// Report whether the probe posts AP reads itself.
    ///
    /// An AP read returns the value of the previous AP read on the wire. The
    /// host collects the last value with a read of RDBUFF. When this returns
    /// true, the probe does so, and it returns one value for every read
    /// operation. `SwdPort` then adds no read of RDBUFF, and it inserts no
    /// idle cycles, because such a probe times the transfers itself.
    fn handles_ap_pipeline(&self) -> bool {
        false
    }

    /// Returns the SWD wire-protocol timing settings used by this probe.
    fn swd_settings(&self) -> SwdSettings {
        SwdSettings::default()
    }
}

/// Bit-banging SWD interface for probe drivers.
pub trait BitbangSwd: DebugProbe {
    /// Drive a sequence of SWD I/O items and return the sampled bits.
    fn swd_io<S>(&mut self, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        S: IntoIterator<Item = IoSequenceItem>;

    /// Returns the SWD wire-protocol timing settings used by this probe.
    fn swd_settings(&self) -> &SwdSettings;

    /// Drive CMSIS-DAP SWJ pins.
    fn swj_pins_op(
        &mut self,
        _out: Pins,
        _select: Pins,
        _wait: Duration,
    ) -> Result<(), DebugProbeError> {
        Err(DebugProbeError::CommandNotSupportedByProbe {
            command_name: "swj_pins",
        })
    }
}

/// A transfer whose response the next [`BitbangSwd::swd_io`] call returns.
struct PendingTransfer {
    id: HandleId,
    direction: Direction,
    /// Offset of the request in the pending I/O sequence.
    offset: usize,
    /// Index of the operation in the batch.
    operation: usize,
}

/// One run of consecutive wire operations, sent as a single `swd_io` call.
///
/// A probe driver may hold turnaround state between the items of one call, so
/// a transfer and the idle cycles that follow it must stay in the same call.
#[derive(Default)]
struct BitbangRun {
    items: Vec<IoSequenceItem>,
    transfers: Vec<PendingTransfer>,
    first_operation: usize,
}

impl BitbangRun {
    fn flush<P: BitbangSwd>(
        &mut self,
        probe: &mut P,
        results: &mut Results,
    ) -> Result<(), BatchExecutionError<DebugProbeError>> {
        if self.items.is_empty() {
            self.transfers.clear();
            return Ok(());
        }

        let expected = self.items.len();
        let transfers = std::mem::take(&mut self.transfers);
        let first_operation = self.first_operation;

        let response = match probe.swd_io(self.items.drain(..)) {
            Ok(response) => response,
            Err(error) => {
                return Err(BatchExecutionError::new_from_debug_probe_at(
                    error,
                    std::mem::take(results),
                    first_operation,
                ));
            }
        };

        if response.len() < expected {
            return Err(BatchExecutionError::new_from_debug_probe_at(
                DebugProbeError::Other(format!(
                    "The probe captured {} bits, but the sequence needs {expected}",
                    response.len(),
                )),
                std::mem::take(results),
                first_operation,
            ));
        }

        for transfer in transfers {
            let sampled = &response[transfer.offset + REQUEST_BITS..];
            match parse_transfer_response(sampled, transfer.direction) {
                Ok(value) => {
                    if transfer.direction == Direction::Read && transfer.id.should_capture() {
                        results.push(&transfer.id, CommandResult::U32(value));
                    }
                }
                Err(error) => {
                    return Err(BatchExecutionError {
                        error: BatchError::Specific(DebugProbeError::SwdTransfer(error)),
                        results: std::mem::take(results),
                        fault_operation: transfer.operation,
                    });
                }
            }
        }

        Ok(())
    }

    fn extend(&mut self, operation: usize, items: impl IntoIterator<Item = IoSequenceItem>) {
        if self.items.is_empty() {
            self.first_operation = operation;
        }
        self.items.extend(items);
    }
}

fn run_bitbang_batch<P: BitbangSwd>(
    probe: &mut P,
    batch: &SwdBatch,
) -> Result<Results, BatchExecutionError<DebugProbeError>> {
    let mut results = Results::new();
    let mut run = BitbangRun::default();

    for (operation, (id, op)) in batch.iter().enumerate() {
        match op {
            SwdOp::Transfer {
                port,
                addr,
                direction,
                data,
            } => {
                let offset = run.items.len();
                run.extend(
                    operation,
                    transfer_io_sequence(*port, *addr, *direction, *data),
                );
                run.transfers.push(PendingTransfer {
                    id: id.clone(),
                    direction: *direction,
                    offset,
                    operation,
                });
            }
            SwdOp::Sequence(bits) => {
                run.extend(operation, bits.iter().map(IoSequenceItem::Output));
            }
            SwdOp::Idle { cycles } => {
                run.extend(
                    operation,
                    std::iter::repeat_n(IoSequenceItem::Output(false), *cycles as usize),
                );
            }
            SwdOp::Pins { out, select, wait } => {
                run.flush(probe, &mut results)?;
                if let Err(error) = probe.swj_pins_op(*out, *select, *wait) {
                    return Err(BatchExecutionError::new_from_debug_probe_at(
                        error, results, operation,
                    ));
                }
            }
        }
    }

    run.flush(probe, &mut results)?;

    Ok(results)
}

impl<P: BitbangSwd> SwdProbe for P {
    fn run_batch(
        &mut self,
        batch: &SwdBatch,
    ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
        run_bitbang_batch(self, batch)
    }

    fn swd_settings(&self) -> SwdSettings {
        BitbangSwd::swd_settings(self).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::probe::WireProtocol;

    fn request_bits(items: &[IoSequenceItem]) -> Vec<bool> {
        items
            .iter()
            .take_while(|item| matches!(item, IoSequenceItem::Output(_)))
            .map(|item| match item {
                IoSequenceItem::Output(bit) => *bit,
                IoSequenceItem::Input => panic!("unexpected input in request bits"),
            })
            .collect()
    }

    #[test]
    fn batch_of_six_operations() {
        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, 0b0100);
        batch.write(Port::Ap, 0b1000, 0x1234_5678);
        batch.sequence(BitSequence::from_u64(16, 0xE24E));
        batch.idle(8);
        let _ = batch.read(Port::Dp, 0b1100);
        batch.write(Port::Dp, 0b0000, 0xDEAD_BEEF);
        assert_eq!(batch.len(), 6);
    }

    #[derive(Debug)]
    struct RecordingBitbangSwd {
        requests: Vec<Vec<bool>>,
        calls: Vec<Vec<IoSequenceItem>>,
        swd_settings: SwdSettings,
        /// Answer this transfer of a call with FAULT.
        fault_transfer: Option<usize>,
    }

    impl RecordingBitbangSwd {
        fn new() -> Self {
            Self::with_settings(SwdSettings::default())
        }

        fn with_settings(swd_settings: SwdSettings) -> Self {
            Self {
                requests: Vec::new(),
                calls: Vec::new(),
                swd_settings,
                fault_transfer: None,
            }
        }

        /// Offsets of the transfer requests in a coalesced I/O sequence.
        ///
        /// A transfer is the only source of an `Input` item, and its request is
        /// eight `Output` items long.
        fn transfer_offsets(items: &[IoSequenceItem]) -> Vec<usize> {
            let mut offsets = Vec::new();
            let mut index = 0;
            while index + TRANSFER_RESPONSE_BITS <= items.len() {
                if items[index + REQUEST_BITS] == IoSequenceItem::Input {
                    offsets.push(index);
                    index += TRANSFER_RESPONSE_BITS;
                } else {
                    index += 1;
                }
            }
            offsets
        }

        fn respond(&self, items: &[IoSequenceItem]) -> Vec<bool> {
            let mut response = vec![false; items.len()];
            for (index, offset) in Self::transfer_offsets(items).into_iter().enumerate() {
                if self.fault_transfer == Some(index) {
                    response[offset + REQUEST_BITS + 2] = true;
                    continue;
                }
                // The direction bit of the request selects read or write.
                let read = items[offset + 2] == IoSequenceItem::Output(true);
                // ACK OK.
                response[offset + REQUEST_BITS] = true;
                if read {
                    // Value zero, so only the parity bit needs a value.
                    response[offset + REQUEST_BITS + 3 + 32] = false;
                }
            }
            response
        }
    }

    impl DebugProbe for RecordingBitbangSwd {
        fn get_name(&self) -> &str {
            "recording bitbang swd"
        }

        fn speed_khz(&self) -> u32 {
            0
        }

        fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
            Ok(speed_khz)
        }

        fn attach(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), crate::Error> {
            Ok(())
        }

        fn target_reset(&mut self) -> Result<(), DebugProbeError> {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset",
            })
        }

        fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "target_reset_assert",
            })
        }

        fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn select_protocol(&mut self, _protocol: WireProtocol) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn active_protocol(&self) -> Option<WireProtocol> {
            Some(WireProtocol::Swd)
        }

        fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
            self
        }
    }

    impl BitbangSwd for RecordingBitbangSwd {
        fn swd_io<S>(&mut self, swdio: S) -> Result<Vec<bool>, DebugProbeError>
        where
            S: IntoIterator<Item = IoSequenceItem>,
        {
            let items = swdio.into_iter().collect::<Vec<_>>();
            self.requests.push(request_bits(&items));
            let response = self.respond(&items);
            self.calls.push(items);

            Ok(response)
        }

        fn swd_settings(&self) -> &SwdSettings {
            &self.swd_settings
        }
    }

    fn run_transfer(probe: &mut RecordingBitbangSwd, op: SwdOp) -> Vec<bool> {
        let mut batch = SwdBatch::new();
        let _ = batch.schedule(op);
        SwdProbe::run_batch(probe, &batch).unwrap();
        probe.requests.pop().unwrap()
    }

    #[test]
    fn lowering_request_bits_match_legacy_encoding() {
        let mut probe = RecordingBitbangSwd::new();

        assert_eq!(
            run_transfer(
                &mut probe,
                SwdOp::Transfer {
                    port: Port::Dp,
                    addr: 0b0100,
                    direction: Direction::Read,
                    data: 0,
                }
            ),
            vec![true, false, true, true, false, false, false, true]
        );

        assert_eq!(
            run_transfer(
                &mut probe,
                SwdOp::Transfer {
                    port: Port::Dp,
                    addr: 0b0100,
                    direction: Direction::Write,
                    data: 0x1234_5678,
                }
            ),
            vec![true, false, false, true, false, true, false, true]
        );

        assert_eq!(
            run_transfer(
                &mut probe,
                SwdOp::Transfer {
                    port: Port::Ap,
                    addr: 0b1100,
                    direction: Direction::Read,
                    data: 0,
                }
            ),
            vec![true, true, true, true, true, false, false, true]
        );

        assert_eq!(
            run_transfer(
                &mut probe,
                SwdOp::Transfer {
                    port: Port::Ap,
                    addr: 0b1100,
                    direction: Direction::Write,
                    data: 0xABCD_EF01,
                }
            ),
            vec![true, true, false, true, true, true, false, true]
        );
    }

    #[test]
    fn a_transfer_and_its_idle_cycles_share_one_swd_io_call() {
        let mut probe = RecordingBitbangSwd::new();

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, 0b0100);
        batch.idle(8);
        SwdProbe::run_batch(&mut probe, &batch).unwrap();

        assert_eq!(probe.calls.len(), 1);
        assert_eq!(probe.calls[0].len(), TRANSFER_RESPONSE_BITS + 8);
        assert_eq!(
            probe.calls[0].last(),
            Some(&IoSequenceItem::Output(false)),
            "the call must end with the idle cycles, so the line stays driven"
        );
    }

    #[test]
    fn a_whole_port_transaction_is_one_swd_io_call() {
        let mut probe = RecordingBitbangSwd::new();

        {
            let mut port = SwdPort::new(&mut probe, SwdSettings::default());
            let mut batch = SwdBatch::new();
            batch.write(Port::Ap, 0b1000, 0x1234_5678);
            port.run(batch).unwrap();
        }

        // AP write, num_idle_cycles_between_writes, idle_cycles_before_write_verify,
        // RDBUFF read, idle_cycles_after_transfer.
        assert_eq!(probe.calls.len(), 1);
        assert_eq!(probe.calls[0].len(), 2 * TRANSFER_RESPONSE_BITS + 2 + 8 + 8);
    }

    #[test]
    fn a_pins_operation_splits_the_swd_io_calls() {
        let mut probe = RecordingBitbangSwd::new();

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, 0b0100);
        batch.idle(8);
        let _ = batch.schedule(SwdOp::Pins {
            out: Pins(0),
            select: Pins(0),
            wait: Duration::ZERO,
        });
        let _ = batch.read(Port::Dp, 0b0100);
        batch.idle(8);
        let error = SwdProbe::run_batch(&mut probe, &batch).unwrap_err();

        // The recording probe does not support pins, so the batch stops there.
        assert_eq!(error.fault_operation, 2);
        assert_eq!(probe.calls.len(), 1);
    }

    #[test]
    fn a_faulting_transfer_reports_its_batch_index() {
        let mut probe = RecordingBitbangSwd::new();
        probe.fault_transfer = Some(1);

        let mut batch = SwdBatch::new();
        let _ = batch.read(Port::Dp, 0b0100);
        batch.idle(4);
        let first = batch.read(Port::Dp, 0b0100);
        let _ = batch.read(Port::Dp, 0b0100);
        let error = SwdProbe::run_batch(&mut probe, &batch).unwrap_err();

        assert_eq!(error.fault_operation, 2);
        assert!(matches!(
            error.error,
            BatchError::Specific(DebugProbeError::SwdTransfer(
                SwdTransferError::FaultResponse
            ))
        ));
        // The transfers before the fault keep their results.
        assert_eq!(error.results.len(), 1);
        drop(first);
    }

    #[test]
    fn bitbang_settings_reach_the_swd_probe_layer() {
        let probe = RecordingBitbangSwd::with_settings(SwdSettings {
            num_idle_cycles_between_writes: 3,
            num_retries_after_wait: 7,
            max_retry_idle_cycles_after_wait: 11,
            idle_cycles_before_write_verify: 13,
            idle_cycles_after_transfer: 17,
        });

        let settings = SwdProbe::swd_settings(&probe);

        assert_eq!(settings.num_idle_cycles_between_writes, 3);
        assert_eq!(settings.num_retries_after_wait, 7);
        assert_eq!(settings.max_retry_idle_cycles_after_wait, 11);
        assert_eq!(settings.idle_cycles_before_write_verify, 13);
        assert_eq!(settings.idle_cycles_after_transfer, 17);
    }
}
