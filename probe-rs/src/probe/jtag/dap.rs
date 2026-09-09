//! ADIv5 DAP register access over JTAG.

use bitvec::{field::BitField, slice::BitSlice};

use crate::{
    architecture::arm::{
        ArmError, DapError, RegisterAddress,
        dp::{Abort, Ctrl, DpRegister, RdBuff},
    },
    probe::{
        BitSequence, CommandResult, DebugProbeError, JtagBatch, JtagChain, JtagChainAccess,
        JtagWriteCommand, JtagWriteData, SwdSettings, TapState,
        queue::{BatchError, JtagQueue},
    },
};

// Constant to be written to ABORT
const JTAG_ABORT_VALUE: u64 = 0x8;

// IR values for JTAG registers
const JTAG_ABORT_IR_VALUE: u32 = 0x8; // A DAP abort, compatible with DPv0
const JTAG_DEBUG_PORT_IR_VALUE: u32 = 0xA;
const JTAG_ACCESS_PORT_IR_VALUE: u32 = 0xB;

const JTAG_STATUS_WAIT: u32 = 0x1;
/// OK/FAULT response
const JTAG_STATUS_OK: u32 = 0x2;

// ARM DR accesses are always 35 bits wide
const JTAG_DR_BIT_LENGTH: u32 = 35;

// Build a JTAG payload
fn build_jtag_payload_and_address(transfer: &DapTransfer) -> (u64, u32) {
    if transfer.is_abort() {
        (JTAG_ABORT_VALUE, JTAG_ABORT_IR_VALUE)
    } else {
        let address = match transfer.address.is_ap() {
            false => JTAG_DEBUG_PORT_IR_VALUE,
            true => JTAG_ACCESS_PORT_IR_VALUE,
        };

        let port_address = transfer.address.a2_and_3();
        let mut payload = 0u64;

        // 32-bit value, bits 35:3
        payload |= (transfer.value as u64) << 3;
        // A[3:2], bits 2:1
        payload |= (port_address as u64 & 0b1000) >> 1;
        payload |= (port_address as u64 & 0b0100) >> 1;
        // RnW, bit 0
        payload |= u64::from(transfer.direction == TransferDirection::Read);

        (payload, address)
    }
}

fn parse_jtag_response(data: &BitSlice) -> u64 {
    data.load_le::<u64>()
}

/// Perform a single JTAG transfer and parse the results
///
/// Return is (value, status)
fn perform_jtag_transfer(
    chain: &mut JtagChain<'_>,
    transfer: &DapTransfer,
) -> Result<(u32, TransferStatus), DebugProbeError> {
    let (payload, address) = build_jtag_payload_and_address(transfer);
    let ir_len = chain.params().irlen;
    let mut batch = JtagBatch::new();
    let ir = BitSequence::from_bytes(&address.to_le_bytes(), ir_len);
    chain.shift_ir(&mut batch, &ir);
    let dr = BitSequence::from_bytes(&payload.to_le_bytes(), JTAG_DR_BIT_LENGTH as usize);
    let handle = chain.exchange_dr(&mut batch, &dr);
    chain.run_test_idle(&mut batch, transfer.idle_cycles_after.min(255) as u32);
    let mut results = chain.run(batch)?;
    let response = results
        .take(handle)
        .map_err(|_| DebugProbeError::Other("missing JTAG capture result".into()))?;
    let received = response.as_bits().load_le::<u64>();

    if transfer.is_abort() {
        // No responses returned from this
        return Ok((0, TransferStatus::Ok));
    }

    // Received value is bits [35:3]
    let received_value = (received >> 3) as u32;
    // Status is bits [2:0]
    let status = (received & 0b111) as u32;

    let transfer_status = match status {
        s if s == JTAG_STATUS_WAIT => TransferStatus::Failed(DapError::WaitResponse),
        s if s == JTAG_STATUS_OK => TransferStatus::Ok,
        _ => {
            tracing::debug!("Unexpected DAP response: {}", status);

            TransferStatus::Failed(DapError::NoAcknowledge)
        }
    };

    Ok((received_value, transfer_status))
}

/// Perform a batch of JTAG transfers in one wire batch.
fn perform_jtag_transfers(
    chain: &mut JtagChain<'_>,
    transfers: &mut [DapTransfer],
) -> Result<(), DebugProbeError> {
    let mut queue: JtagQueue<DapError> = JtagQueue::new();

    let mut results: Vec<_> = transfers
        .iter()
        .map(|t| queue.schedule(t.jtag_write()))
        .collect();

    let last_is_abort = transfers[transfers.len() - 1].is_abort();
    let last_is_rdbuff = transfers[transfers.len() - 1].is_rdbuff();
    if !last_is_abort && !last_is_rdbuff {
        // Need to issue a fake read to get final ack
        results.push(queue.schedule(DapTransfer::read(RdBuff::ADDRESS).jtag_write()));
    }

    if !last_is_abort {
        // Check CTRL/STATUS to make sure OK/FAULT meant OK
        results.push(queue.schedule(DapTransfer::read(Ctrl::ADDRESS).jtag_write()));
        results.push(queue.schedule(DapTransfer::read(RdBuff::ADDRESS).jtag_write()));
    }

    let mut status_responses = vec![TransferStatus::Pending; results.len()];

    // Execute as much of the batch as we can. We'll handle the rest in a following iteration
    // if we can.
    let mut jtag_results;
    match queue.execute(|queue| chain.run_command_batch(queue)) {
        Ok(r) => {
            status_responses.fill(TransferStatus::Ok);
            jtag_results = r;
        }
        Err(e) => {
            let current_idx = e.results.len();
            status_responses[..current_idx].fill(TransferStatus::Ok);
            jtag_results = e.results;

            match e.error {
                BatchError::Specific(failure) => {
                    status_responses[current_idx..].fill(TransferStatus::Failed(failure));
                    jtag_results.push(results[current_idx].id(), CommandResult::None);
                }
                BatchError::Probe(err) => {
                    return Err(err);
                }
            }
        }
    }

    // Process the results. At this point we should only have OK/FAULT responses.
    for (i, transfer) in transfers.iter_mut().enumerate() {
        transfer.status = *status_responses.get(i + 1).unwrap_or(&TransferStatus::Ok);
    }

    // Pluck off the extra 2 results that do error checking
    let ctrl_value = if !last_is_abort {
        _ = results
            .pop()
            .expect("Failed to pop value that was pushed here.");
        let rdbuff_result = results
            .pop()
            .expect("Failed to pop value that was pushed here.");

        Some(rdbuff_result)
    } else {
        None
    };

    // Shift the results.
    // Each response is read in the next transaction, so skip 1
    for (i, result) in results.into_iter().skip(1).enumerate() {
        let transfer = &mut transfers[i];
        if transfer.is_abort() || transfer.is_rdbuff() {
            transfer.status = TransferStatus::Ok;
            continue;
        }

        if transfer.status == TransferStatus::Ok && transfer.direction == TransferDirection::Read {
            let response = jtag_results.take(result).unwrap();
            transfer.value = response.into_u32();
        }
    }

    if let Some(ctrl_value) = ctrl_value {
        // Check CTRL/STATUS to make sure OK/FAULT meant OK
        if let Ok(CommandResult::U32(received_value)) = jtag_results.take(ctrl_value)
            && Ctrl(received_value).sticky_err()
        {
            tracing::debug!("JTAG transaction set failed: {:#X?}", transfers);

            // Clear the sticky bit so future transactions succeed
            let (_, _) =
                perform_jtag_transfer(chain, &DapTransfer::write(Ctrl::ADDRESS, received_value))?;

            // Mark OK/FAULT transactions as failed. Since the error is sticky, we can assume that
            // if we received a WAIT, the previous transactions were successful.
            // The caller will reset the sticky flag and retry if needed
            for transfer in transfers.iter_mut() {
                if transfer.status == TransferStatus::Ok {
                    transfer.status = TransferStatus::Failed(DapError::FaultResponse);
                }
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct DapTransfer {
    address: RegisterAddress,
    direction: TransferDirection,
    value: u32,
    status: TransferStatus,
    idle_cycles_after: usize,
}

impl DapTransfer {
    fn read<P: Into<RegisterAddress>>(address: P) -> DapTransfer {
        Self {
            address: address.into(),
            direction: TransferDirection::Read,
            value: 0,
            status: TransferStatus::Pending,
            idle_cycles_after: 0,
        }
    }

    fn write<P: Into<RegisterAddress>>(address: P, value: u32) -> DapTransfer {
        Self {
            address: address.into(),
            value,
            direction: TransferDirection::Write,
            status: TransferStatus::Pending,
            idle_cycles_after: 0,
        }
    }

    fn jtag_write(&self) -> JtagWriteCommand<DapError> {
        let (payload, address) = if self.is_abort() {
            (JTAG_ABORT_VALUE, JTAG_ABORT_IR_VALUE)
        } else {
            let jtag_address = match self.address.is_ap() {
                false => JTAG_DEBUG_PORT_IR_VALUE,
                true => JTAG_ACCESS_PORT_IR_VALUE,
            };
            let port_address = self.address.a2_and_3();

            let mut payload = 0u64;

            // 32-bit value, bits 35:3
            payload |= (self.value as u64) << 3;
            // A[3:2], bits 2:1
            payload |= (port_address as u64 & 0b1000) >> 1;
            payload |= (port_address as u64 & 0b0100) >> 1;
            // RnW, bit 0
            payload |= u64::from(self.direction == TransferDirection::Read);

            (payload, jtag_address)
        };

        JtagWriteCommand {
            data: JtagWriteData {
                address,
                data: BitSequence::from_bytes(&payload.to_le_bytes(), JTAG_DR_BIT_LENGTH as usize),
                idle_cycles: self.idle_cycles_after.min(255) as u32,
            },
            transform: |data, response| {
                // No responses returned for aborts.
                if data.address == JTAG_ABORT_IR_VALUE {
                    return Ok(CommandResult::None);
                }

                let received = parse_jtag_response(response);

                // Received value is bits [35:3]
                let received_value = (received >> 3) as u32;
                // Status is bits [2:0]
                let status = (received & 0b111) as u32;

                let error = match status {
                    s if s == JTAG_STATUS_OK => return Ok(CommandResult::U32(received_value)),
                    s if s == JTAG_STATUS_WAIT => DapError::WaitResponse,
                    _ => {
                        tracing::debug!("Unexpected DAP response: {}", status);

                        DapError::NoAcknowledge
                    }
                };

                Err(error)
            },
        }
    }

    fn is_abort(&self) -> bool {
        matches!(self.address, RegisterAddress::DpRegister(Abort::ADDRESS))
            && self.direction == TransferDirection::Write
    }

    fn is_rdbuff(&self) -> bool {
        matches!(self.address, RegisterAddress::DpRegister(RdBuff::ADDRESS))
            && self.direction == TransferDirection::Read
    }
}

#[derive(Debug, PartialEq, Copy, Clone)]
enum TransferDirection {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum TransferStatus {
    Pending,
    /// OK/FAULT response
    Ok,
    Failed(DapError),
}

fn clear_overrun_and_sticky_err(chain: &mut JtagChain<'_>) -> Result<(), DebugProbeError> {
    tracing::debug!("Clearing overrun and sticky error");
    let mut abort = Abort(0);
    abort.set_orunerrclr(true);
    abort.set_stkerrclr(true);
    let transfer = DapTransfer::write(Abort::ADDRESS, abort.into());
    perform_jtag_transfer(chain, &transfer)?;
    Ok(())
}

fn perform_jtag_transfers_with_retry(
    chain: &mut JtagChain<'_>,
    transfers: &mut [DapTransfer],
    settings: &SwdSettings,
) -> Result<(), DebugProbeError> {
    let mut successful_transfers = 0;
    let mut idle_cycles = settings.num_idle_cycles_between_writes.max(1);
    let num_retries = settings.num_retries_after_wait;

    'transfer: for _ in 0..num_retries {
        let chunk = &mut transfers[successful_transfers..];
        if chunk.is_empty() {
            return Ok(());
        }

        perform_jtag_transfers(chain, chunk)?;

        for transfer in chunk.iter() {
            match transfer.status {
                TransferStatus::Ok => successful_transfers += 1,
                TransferStatus::Failed(DapError::WaitResponse) => {
                    tracing::debug!("got WAIT on transfer {}, retrying...", successful_transfers);

                    clear_overrun_and_sticky_err(chain).inspect_err(|e| {
                        tracing::error!("error clearing sticky overrun/error bits: {e}");
                    })?;

                    for transfer in chunk.iter_mut() {
                        if transfer.direction == TransferDirection::Write {
                            transfer.idle_cycles_after += idle_cycles;
                        }
                    }
                    idle_cycles = idle_cycles
                        .saturating_mul(2)
                        .min(settings.max_retry_idle_cycles_after_wait);

                    continue 'transfer;
                }
                _ => return Ok(()),
            }
        }

        if successful_transfers == transfers.len() {
            return Ok(());
        }
    }

    tracing::debug!(
        "Timeout in JTAG transaction, aborting AP transactions after {num_retries} retries."
    );
    let mut abort = Abort(0);
    abort.set_dapabort(true);
    let transfer = DapTransfer::write(Abort::ADDRESS, abort.into());
    perform_jtag_transfer(chain, &transfer)?;

    Ok(())
}

pub(crate) fn jtag_output_sequence(
    probe: &mut dyn JtagChainAccess,
    tms: bool,
    tdi: &BitSequence,
) -> Result<(), DebugProbeError> {
    if tms {
        shift_tms_bits(probe, true, tdi.len())?;
        return Ok(());
    }

    if tdi.len() == 1 {
        shift_tms_bits(probe, false, 1)?;
        return Ok(());
    }

    let mut chain = JtagChain::new(probe);
    let mut batch = JtagBatch::new();
    batch.enter(TapState::ShiftDr);
    batch.exchange_no_capture(tdi.clone());
    batch.enter(TapState::RunTestIdle);
    chain.run(batch)?;
    Ok(())
}

fn shift_tms_bits(
    probe: &mut dyn JtagChainAccess,
    tms: bool,
    bit_count: usize,
) -> Result<(), DebugProbeError> {
    let start = probe.chain_state_ref().tap_state;
    let end = stable_state_after_tms(start, tms, bit_count)?;
    let mut chain = JtagChain::new(probe);
    let mut batch = JtagBatch::new();
    batch.enter(end);
    chain.run(batch)?;
    Ok(())
}

fn stable_state_after_tms(
    start: TapState,
    tms: bool,
    bit_count: usize,
) -> Result<TapState, DebugProbeError> {
    let mut state = FullTapState::from_stable(start);
    for _ in 0..bit_count {
        state = state.step(tms);
    }
    state.to_stable()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FullTapState {
    TestLogicReset,
    RunTestIdle,
    SelectDr,
    CaptureDr,
    ShiftDr,
    Exit1Dr,
    PauseDr,
    Exit2Dr,
    UpdateDr,
    SelectIr,
    CaptureIr,
    ShiftIr,
    Exit1Ir,
    PauseIr,
    Exit2Ir,
    UpdateIr,
}

impl FullTapState {
    fn from_stable(state: TapState) -> Self {
        match state {
            TapState::TestLogicReset => Self::TestLogicReset,
            TapState::RunTestIdle => Self::RunTestIdle,
            TapState::ShiftIr => Self::ShiftIr,
            TapState::ShiftDr => Self::ShiftDr,
            TapState::PauseIr => Self::PauseIr,
            TapState::PauseDr => Self::PauseDr,
        }
    }

    fn step(self, tms: bool) -> Self {
        if tms {
            match self {
                Self::TestLogicReset => Self::TestLogicReset,
                Self::RunTestIdle => Self::SelectDr,
                Self::SelectDr => Self::SelectIr,
                Self::CaptureDr | Self::ShiftDr => Self::Exit1Dr,
                Self::Exit1Dr | Self::Exit2Dr => Self::UpdateDr,
                Self::UpdateDr => Self::SelectDr,
                Self::SelectIr => Self::CaptureIr,
                Self::CaptureIr | Self::ShiftIr => Self::Exit1Ir,
                Self::Exit1Ir | Self::Exit2Ir => Self::UpdateIr,
                Self::UpdateIr => Self::SelectDr,
                Self::PauseDr => Self::Exit2Dr,
                Self::PauseIr => Self::Exit2Ir,
            }
        } else {
            match self {
                Self::TestLogicReset => Self::RunTestIdle,
                Self::RunTestIdle => Self::RunTestIdle,
                Self::SelectDr => Self::CaptureDr,
                Self::CaptureDr | Self::ShiftDr => Self::ShiftDr,
                Self::Exit1Dr => Self::PauseDr,
                Self::PauseDr => Self::PauseDr,
                Self::Exit2Dr => Self::ShiftDr,
                Self::UpdateDr => Self::RunTestIdle,
                Self::SelectIr => Self::CaptureIr,
                Self::CaptureIr | Self::ShiftIr => Self::ShiftIr,
                Self::Exit1Ir => Self::PauseIr,
                Self::PauseIr => Self::PauseIr,
                Self::Exit2Ir => Self::ShiftIr,
                Self::UpdateIr => Self::RunTestIdle,
            }
        }
    }

    fn to_stable(self) -> Result<TapState, DebugProbeError> {
        match self {
            Self::TestLogicReset => Ok(TapState::TestLogicReset),
            Self::RunTestIdle => Ok(TapState::RunTestIdle),
            Self::ShiftIr => Ok(TapState::ShiftIr),
            Self::ShiftDr => Ok(TapState::ShiftDr),
            Self::PauseIr => Ok(TapState::PauseIr),
            Self::PauseDr => Ok(TapState::PauseDr),
            _ => Err(DebugProbeError::Other(
                "SWJ sequence did not end in a stable TAP state".into(),
            )),
        }
    }
}

pub(crate) fn jtag_read_register(
    chain: &mut JtagChain<'_>,
    address: RegisterAddress,
    settings: &SwdSettings,
) -> Result<u32, ArmError> {
    let mut transfer = DapTransfer::read(address);
    perform_jtag_transfers_with_retry(chain, std::slice::from_mut(&mut transfer), settings)?;

    match transfer.status {
        TransferStatus::Ok => Ok(transfer.value),
        TransferStatus::Failed(DapError::FaultResponse) => Err(DapError::FaultResponse.into()),
        TransferStatus::Failed(error) => Err(error.into()),
        other => {
            panic!("Unexpected transfer state after reading register: {other:?}. This is a bug!")
        }
    }
}

pub(crate) fn jtag_write_register(
    chain: &mut JtagChain<'_>,
    address: RegisterAddress,
    value: u32,
    settings: &SwdSettings,
) -> Result<(), ArmError> {
    let mut transfer = DapTransfer::write(address, value);
    perform_jtag_transfers_with_retry(chain, std::slice::from_mut(&mut transfer), settings)?;

    match transfer.status {
        TransferStatus::Ok => Ok(()),
        TransferStatus::Failed(DapError::FaultResponse) => Err(DapError::FaultResponse.into()),
        TransferStatus::Failed(error) => Err(error.into()),
        other => {
            panic!("Unexpected transfer state after writing register: {other:?}. This is a bug!")
        }
    }
}

pub(crate) fn jtag_read_block(
    chain: &mut JtagChain<'_>,
    address: RegisterAddress,
    values: &mut [u32],
    settings: &SwdSettings,
) -> Result<(), ArmError> {
    let mut transfers = vec![DapTransfer::read(address); values.len()];
    perform_jtag_transfers_with_retry(chain, &mut transfers, settings)?;

    for (index, transfer) in transfers.iter().enumerate() {
        match transfer.status {
            TransferStatus::Ok => values[index] = transfer.value,
            TransferStatus::Failed(error) => return Err(error.into()),
            other => panic!(
                "Unexpected transfer state after reading registers: {other:?}. This is a bug!"
            ),
        }
    }

    Ok(())
}

pub(crate) fn jtag_write_block(
    chain: &mut JtagChain<'_>,
    address: RegisterAddress,
    values: &[u32],
    settings: &SwdSettings,
) -> Result<(), ArmError> {
    let mut transfers = values
        .iter()
        .map(|value| DapTransfer::write(address, *value))
        .collect::<Vec<_>>();
    perform_jtag_transfers_with_retry(chain, &mut transfers, settings)?;

    for transfer in &transfers {
        match transfer.status {
            TransferStatus::Ok => {}
            TransferStatus::Failed(error) => return Err(error.into()),
            other => panic!(
                "Unexpected transfer state after writing registers: {other:?}. This is a bug!"
            ),
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        JTAG_ABORT_IR_VALUE, JTAG_ACCESS_PORT_IR_VALUE, JTAG_DEBUG_PORT_IR_VALUE, JTAG_STATUS_OK,
        JTAG_STATUS_WAIT, SwdSettings, jtag_read_register, jtag_write_register,
    };
    use crate::{
        architecture::arm::{
            ApAddress, RegisterAddress,
            dp::{Ctrl, DpRegister, RdBuff},
        },
        error::Error,
        probe::{
            BatchExecutionError, CommandResult, DebugProbe, DebugProbeError, JtagBatch, JtagChain,
            JtagChainAccess, JtagChainState, JtagOp, JtagProbe, Results, WireProtocol,
            jtag::chain::ChainParams,
        },
    };
    use bitvec::prelude::*;

    #[expect(dead_code)]
    enum DapAcknowledge {
        Ok,
        Wait,
        Fault,
        NoAck,
    }

    #[derive(Debug)]
    struct ExpectedJtagTransaction {
        ir_address: u32,
        address: u32,
        value: u32,
        read: bool,
        result: u64,
    }

    #[derive(Debug)]
    struct MockJaylink {
        jtag_transactions: Vec<ExpectedJtagTransaction>,
        expected_transfer_count: usize,
        performed_transfer_count: usize,
        protocol: WireProtocol,
        jtag_state: JtagChainState,
    }

    impl MockJaylink {
        fn new() -> Self {
            Self {
                jtag_transactions: vec![],
                expected_transfer_count: 0,
                performed_transfer_count: 0,
                protocol: WireProtocol::Swd,
                jtag_state: JtagChainState {
                    chain_params: ChainParams {
                        irlen: 4,
                        ..ChainParams::default()
                    },
                    ..JtagChainState::default()
                },
            }
        }

        fn add_jtag_abort(&mut self) {
            self.jtag_transactions.push(ExpectedJtagTransaction {
                ir_address: JTAG_ABORT_IR_VALUE,
                address: 0,
                value: 0,
                read: false,
                result: 0,
            });
            self.expected_transfer_count += 1;
        }

        fn add_jtag_response<P: Into<RegisterAddress>>(
            &mut self,
            address: P,
            read: bool,
            acknowledge: DapAcknowledge,
            output_value: u32,
            input_value: u32,
        ) {
            let port = address.into();
            let address = port.lsb().into();
            let mut response = (output_value as u64) << 3;

            let status = match acknowledge {
                DapAcknowledge::Ok => JTAG_STATUS_OK,
                DapAcknowledge::Wait => JTAG_STATUS_WAIT,
                _ => 0b111,
            };

            response |= status as u64;

            self.jtag_transactions.push(ExpectedJtagTransaction {
                ir_address: if matches!(port, RegisterAddress::DpRegister(_)) {
                    JTAG_DEBUG_PORT_IR_VALUE
                } else {
                    JTAG_ACCESS_PORT_IR_VALUE
                },
                address,
                value: input_value,
                read,
                result: response,
            });
            self.expected_transfer_count += 1;
        }
    }

    impl JtagChainAccess for MockJaylink {
        fn chain_state(&mut self) -> &mut JtagChainState {
            &mut self.jtag_state
        }

        fn chain_state_ref(&self) -> &JtagChainState {
            &self.jtag_state
        }
    }

    impl JtagProbe for MockJaylink {
        fn run_batch(
            &mut self,
            batch: &JtagBatch,
        ) -> Result<Results, BatchExecutionError<DebugProbeError>> {
            let mut results = Results::new();
            let mut pending_ir: Option<u32> = None;

            for (id, op) in batch.iter() {
                if let JtagOp::Exchange { data, capture } = op {
                    if data.len() <= 8 {
                        pending_ir = Some(data.as_bits().load_le::<u32>());
                        continue;
                    }

                    let jtag_transaction = self.jtag_transactions.remove(0);
                    assert_eq!(
                        jtag_transaction.ir_address,
                        pending_ir.unwrap_or(jtag_transaction.ir_address),
                        "Address mismatch with {} remaining transactions",
                        self.jtag_transactions.len()
                    );

                    if jtag_transaction.ir_address != JTAG_ABORT_IR_VALUE {
                        let jtag_value = data.as_bits().load_le::<u64>();
                        let value = (jtag_value >> 3) as u32;
                        let rnw = jtag_value & 1 == 1;
                        let dap_address = ((jtag_value & 0x6) << 1) as u32;

                        assert_eq!(dap_address, jtag_transaction.address);
                        assert_eq!(rnw, jtag_transaction.read);
                        assert_eq!(value, jtag_transaction.value);
                    }

                    self.performed_transfer_count += 1;
                    pending_ir = None;

                    if *capture && id.should_capture() {
                        let ret = jtag_transaction.result;
                        let mut bytes = vec![0u8; 5];
                        bytes[..5].view_bits_mut::<Lsb0>().store_le(ret);
                        results.push(id, CommandResult::VecU8(bytes));
                    }
                }
            }

            Ok(results)
        }
    }

    impl DebugProbe for MockJaylink {
        fn get_name(&self) -> &str {
            "mock jtag"
        }

        fn speed_khz(&self) -> u32 {
            0
        }

        fn set_speed(&mut self, _speed_khz: u32) -> Result<u32, DebugProbeError> {
            Ok(0)
        }

        fn attach(&mut self) -> Result<(), DebugProbeError> {
            Ok(())
        }

        fn detach(&mut self) -> Result<(), Error> {
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

        fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
            self.protocol = protocol;
            Ok(())
        }

        fn active_protocol(&self) -> Option<WireProtocol> {
            Some(self.protocol)
        }

        fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
            self
        }
    }

    #[test]
    fn read_register_jtag() {
        let read_value = 12;
        let mut mock = MockJaylink::new();
        mock.select_protocol(WireProtocol::Jtag).unwrap();

        mock.add_jtag_response(ApAddress::V1(4), true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, read_value, 0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        let mut chain = JtagChain::new(&mut mock);
        let result =
            jtag_read_register(&mut chain, ApAddress::V1(4).into(), &SwdSettings::default())
                .expect("read should succeed");

        assert_eq!(result, read_value);
        assert_eq!(mock.performed_transfer_count, mock.expected_transfer_count);
    }

    #[test]
    fn read_register_with_wait_response_jtag() {
        let read_value = 47;
        let mut mock = MockJaylink::new();
        mock.select_protocol(WireProtocol::Jtag).unwrap();

        mock.add_jtag_response(ApAddress::V1(4), true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Wait, 0, 0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        mock.add_jtag_abort();

        mock.add_jtag_response(ApAddress::V1(4), true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, read_value, 0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        let mut chain = JtagChain::new(&mut mock);
        let result =
            jtag_read_register(&mut chain, ApAddress::V1(4).into(), &SwdSettings::default())
                .expect("read should succeed");

        assert_eq!(result, read_value);
        assert_eq!(mock.performed_transfer_count, mock.expected_transfer_count);
    }

    #[test]
    fn write_register_jtag() {
        let mut mock = MockJaylink::new();
        mock.select_protocol(WireProtocol::Jtag).unwrap();

        mock.add_jtag_response(ApAddress::V1(4), false, DapAcknowledge::Ok, 0x0, 0x123);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0x123, 0x0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        let mut chain = JtagChain::new(&mut mock);
        jtag_write_register(
            &mut chain,
            ApAddress::V1(4).into(),
            0x123,
            &SwdSettings::default(),
        )
        .expect("write should succeed");

        assert_eq!(mock.performed_transfer_count, mock.expected_transfer_count);
    }

    #[test]
    fn write_register_with_wait_response_jtag() {
        let mut mock = MockJaylink::new();
        mock.select_protocol(WireProtocol::Jtag).unwrap();

        mock.add_jtag_response(ApAddress::V1(4), false, DapAcknowledge::Ok, 0x0, 0x123);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Wait, 0x0, 0x0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        mock.add_jtag_abort();

        mock.add_jtag_response(ApAddress::V1(4), false, DapAcknowledge::Ok, 0x0, 0x123);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0x123, 0x0);
        mock.add_jtag_response(Ctrl::ADDRESS, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(RdBuff::ADDRESS, true, DapAcknowledge::Ok, 0, 0);

        let mut chain = JtagChain::new(&mut mock);
        jtag_write_register(
            &mut chain,
            ApAddress::V1(4).into(),
            0x123,
            &SwdSettings::default(),
        )
        .expect("write should succeed");

        assert_eq!(mock.performed_transfer_count, mock.expected_transfer_count);
    }
}
