use std::iter;

use crate::{
    architecture::arm::{
        dp::{Abort, Ctrl, RdBuff, DPIDR},
        DapError, DpAddress, PortType, RawDapAccess, Register,
    },
    DebugProbeError,
};

use super::{bits_to_byte, JLink};

#[derive(Debug)]
pub struct SwdSettings {
    /// Initial number of idle cycles between consecutive writes.
    ///
    /// When a WAIT response is received, the number of idle cycles
    /// will be increased automatically, so this number can be quite
    /// low.
    num_idle_cycles_between_writes: usize,

    /// How often a SWD transfer is retried when a WAIT response
    /// is received.
    num_retries_after_wait: usize,

    /// When a SWD transfer is retried due to a WAIT response, the idle
    /// cycle amount is doubled every time as a backoff. This sets a maximum
    /// cap to the cycle amount.
    max_retry_idle_cycles_after_wait: usize,

    /// Number of idle cycles inserted before the result
    /// of a write is checked.
    ///
    /// When performing a write operation, the write can
    /// be buffered, meaning that completing the transfer
    /// does not mean that the write was performed succesfully.
    ///
    /// To check that all writes have been executed, the
    /// `RDBUFF` register can be read from the DP.
    ///
    /// If any writes are still pending, this read will result in a WAIT response.
    /// By adding idle cycles before performing this read, the chance of a
    /// WAIT response is smaller.
    idle_cycles_before_write_verify: usize,

    /// Number of idle cycles to insert after a transfer
    ///
    /// It is recommended that at least 8 idle cycles are
    /// inserted.
    idle_cycles_after_transfer: usize,
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

#[derive(Default, Debug)]
pub struct SwdStatistics {
    /// Number of SWD transfers performed.
    ///
    /// This includes repeated transfers, and transfers
    /// which are automatically added to fullfill
    /// protocol requirements, e.g. a read from a
    /// DP register will result in two transfers,
    /// because the read value is returned in the
    /// second transfer
    num_transfers: usize,

    /// Number of extra transfers added to fullfil protocol
    /// requirements. Ideally as low as possible.
    num_extra_transfers: usize,

    /// Number of calls to the probe IO function.
    ///
    /// A single call can perform multiple SWD transfers,
    /// so this number is ideally a lot lower than then
    /// number of SWD transfers.
    num_io_calls: usize,

    /// Number of SWD wait responses encountered.
    num_wait_resp: usize,

    /// Number of SWD FAULT responses encountered.
    num_faults: usize,

    /// Number of line resets executed.
    num_line_resets: usize,
}

impl SwdStatistics {
    fn record_extra_transfer(&mut self) {
        self.num_extra_transfers += 1;
    }

    fn record_transfers(&mut self, num_transfers: usize) {
        self.num_transfers += num_transfers;
    }

    fn report_io(&mut self) {
        self.num_io_calls += 1;
    }

    fn report_swd_response<T>(&mut self, response: &Result<T, DapError>) {
        match response {
            Err(DapError::FaultResponse) => self.num_faults += 1,
            Err(DapError::WaitResponse) => self.num_wait_resp += 1,
            // Other errors are not counted right now.
            _ => (),
        }
    }

    fn report_line_reset(&mut self) {
        self.num_line_resets += 1;
    }
}

///! Implementation of the SWD protocol for the JLink probe.

/// Perform a batch of SWD transfers.
///
/// For each transfer, the corresponding bit sequence is
/// created and the resulting sequences are concatened
/// to a single sequence, so that it can be sent to
/// to the probe.
///
/// Certain transfers require additional transfers to
/// get the result. This is handled by this function.
fn perform_transfers<P: RawSwdIo>(
    probe: &mut P,
    transfers: &mut [SwdTransfer],
    idle_cycles: usize,
) -> Result<(), DebugProbeError> {
    assert!(!transfers.is_empty());

    // Read from DebugPort  -> Nothing special needed
    // Read from AccessPort -> Response is returned in next read
    //                         -> The next transfer must be a AP Read, otherwise we need to insert a read from the RDBUFF register
    // Write to any port    -> Status is reported in next transfer
    // Write to any port    -> Writes can be buffered, so certain transfers have to be avoided until a instruction which can be stalled is performed

    // The expected responses, needed for parsing
    let mut expected_responses: Vec<(TransferDirection, usize)> = Vec::new();

    let mut result_indices = Vec::new();

    let mut num_transfers = 0;

    let mut io_sequence = IoSequence::new();

    let mut need_ap_read = false;
    let mut buffered_write = false;
    let mut write_response_pending = false;

    for transfer in transfers.iter() {
        // Check if we need to insert an additional read from the RDBUFF register
        if !transfer.is_ap_read() && need_ap_read {
            io_sequence.extend(&rdbuff_read());
            num_transfers += 1;

            expected_responses.push((TransferDirection::Read, 0));

            // This is an extra transfer, which doesn't have a reponse on it's own.
            probe.swd_statistics().record_extra_transfer();
        }

        if buffered_write {
            // Check if we need an additional instruction to avoid loosing buffered writes.

            let abort_write = transfer.port == PortType::DebugPort
                && transfer.address == Abort::ADDRESS
                && transfer.direction == TransferDirection::Write;

            let dpidr_read = transfer.port == PortType::DebugPort
                && transfer.address == DPIDR::ADDRESS
                && transfer.direction == TransferDirection::Read;

            let ctrl_stat_read = transfer.port == PortType::DebugPort
                && transfer.address == Ctrl::ADDRESS
                && transfer.direction == TransferDirection::Read;

            if abort_write || dpidr_read || ctrl_stat_read {
                io_sequence.add_output_sequence(&vec![
                    false;
                    probe
                        .swd_settings()
                        .idle_cycles_before_write_verify
                ]);

                if let Some((_transfer, extra_cycles)) = expected_responses.last_mut() {
                    *extra_cycles += probe.swd_settings().idle_cycles_before_write_verify
                }

                // Add a read from RDBUFF, this access will stalled by the DebugPort if the write buffer
                // is not empty.
                io_sequence.extend(&rdbuff_read());

                num_transfers += 1;
                expected_responses.push((TransferDirection::Read, 0));

                // This is an extra transfer, which doesn't have a reponse on it's own.
                probe.swd_statistics().record_extra_transfer();
            }
        }

        io_sequence.extend(&transfer.io_sequence());

        // The response for an AP read is returned in the next response
        need_ap_read = transfer.is_ap_read();

        // Writes to the AP can be buffered
        //
        // TODO: Can DP writes be buffered as well?
        buffered_write =
            transfer.port == PortType::AccessPort && transfer.direction == TransferDirection::Write;

        // For all writes, except writes to the DP ABORT register, we need to perform another register to ensure that
        // we know if the write succeeded.
        write_response_pending = transfer.is_write()
            && !(matches!(transfer.port, PortType::DebugPort)
                && transfer.address == Abort::ADDRESS);

        // If the response is returned in the next transfer, we push the correct index
        // if need_ap_read || write_response_pending {
        if need_ap_read || write_response_pending {
            result_indices.push(num_transfers + 1);
        } else {
            result_indices.push(num_transfers);
        }

        expected_responses.push((transfer.direction, 0));

        if transfer.is_write() {
            log::trace!("Adding {} idle cycles after transfer!", idle_cycles);

            io_sequence.add_output_sequence(&vec![false; idle_cycles]);

            expected_responses.last_mut().unwrap().1 = idle_cycles;
        }

        num_transfers += 1;
    }

    //if need_ap_read || write_response_pending {
    if need_ap_read || write_response_pending {
        if write_response_pending {
            io_sequence.add_output_sequence(&vec![
                false;
                probe
                    .swd_settings()
                    .idle_cycles_before_write_verify
            ]);

            if let Some((_transfer, extra_cycles)) = expected_responses.last_mut() {
                *extra_cycles += probe.swd_settings().idle_cycles_before_write_verify;
            }
        }

        io_sequence.extend(&rdbuff_read());

        expected_responses.push((TransferDirection::Read, 0));

        num_transfers += 1;
        probe.swd_statistics().record_extra_transfer();
    }

    // Add idle cycles at the end, to ensure transfer is performed
    if probe.swd_settings().idle_cycles_after_transfer > 0 {
        io_sequence.add_output_sequence(&vec![
            false;
            probe.swd_settings().idle_cycles_after_transfer
        ]);
    }

    log::debug!(
        "Performing {} transfers ({} additional transfers)",
        num_transfers,
        num_transfers - transfers.len()
    );

    probe.swd_statistics().record_transfers(num_transfers);

    let result = probe.swd_io(
        io_sequence.direction_bits().to_owned(),
        io_sequence.io_bits().to_owned(),
    )?;

    // Parse the response

    let mut responses = Vec::with_capacity(num_transfers);

    let mut read_index = 0;

    for (i, (response_direction, additional_idle_cycles_after)) in
        expected_responses.into_iter().enumerate()
    {
        let response = parse_swd_response(&result[read_index..], response_direction);

        probe.swd_statistics().report_swd_response(&response);

        log::debug!("Transfer result {}: {:x?}", i, response);

        responses.push(response);

        read_index += response_length(response_direction);

        read_index += additional_idle_cycles_after;
    }

    let wait_result_index = responses
        .iter()
        .position(|r| r == &Err(DapError::WaitResponse))
        .unwrap_or(responses.len());

    // Retrieve the results
    for (transfer, index) in transfers.iter_mut().zip(result_indices) {
        match &responses[index] {
            Ok(value) => {
                if transfer.direction == TransferDirection::Read {
                    transfer.value = *value;
                }

                transfer.status = TransferStatus::Ok;
            }
            Err(e) => {
                transfer.status = TransferStatus::Failed(if index >= wait_result_index {
                    DapError::WaitResponse
                } else {
                    e.clone()
                });
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct SwdTransfer {
    port: PortType,
    direction: TransferDirection,
    address: u8,
    value: u32,
    status: TransferStatus,
}

impl SwdTransfer {
    fn read(port: PortType, address: u8) -> SwdTransfer {
        Self {
            port,
            address,
            direction: TransferDirection::Read,
            value: 0,
            status: TransferStatus::Pending,
        }
    }

    fn write(port: PortType, address: u8, value: u32) -> SwdTransfer {
        Self {
            port,
            address,
            value,
            direction: TransferDirection::Write,
            status: TransferStatus::Pending,
        }
    }

    fn transfer_type(&self) -> TransferType {
        match self.direction {
            TransferDirection::Read => TransferType::Read,
            TransferDirection::Write => TransferType::Write(self.value),
        }
    }

    fn io_sequence(&self) -> IoSequence {
        build_swd_transfer(self.port, self.transfer_type(), self.address)
    }

    // Helper functions for combining transfers

    fn is_ap_read(&self) -> bool {
        self.port == PortType::AccessPort && self.direction == TransferDirection::Read
    }

    fn is_write(&self) -> bool {
        self.direction == TransferDirection::Write
    }
}

fn rdbuff_read() -> IoSequence {
    SwdTransfer::read(PortType::DebugPort, RdBuff::ADDRESS).io_sequence()
}

#[derive(Debug, PartialEq, Copy, Clone)]
enum TransferDirection {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq)]
enum TransferStatus {
    Pending,
    Ok,
    Failed(DapError),
}

struct IoSequence {
    io: Vec<bool>,
    direction: Vec<bool>,
}

impl IoSequence {
    const INPUT: bool = false;
    const OUTPUT: bool = true;

    fn new() -> Self {
        IoSequence {
            io: vec![],
            direction: vec![],
        }
    }

    fn add_output(&mut self, bit: bool) {
        self.io.push(bit);
        self.direction.push(Self::OUTPUT);
    }

    fn add_output_sequence(&mut self, bits: &[bool]) {
        self.io.extend_from_slice(bits);
        self.direction
            .extend(iter::repeat(Self::OUTPUT).take(bits.len()));
    }

    fn add_input(&mut self) {
        // Input bit, the
        self.io.push(false);
        self.direction.push(Self::INPUT);
    }

    fn add_input_sequence(&mut self, length: usize) {
        // Input bit, the
        self.io.extend(iter::repeat(false).take(length));
        self.direction
            .extend(iter::repeat(Self::INPUT).take(length));
    }

    fn io_bits(&self) -> &[bool] {
        &self.io
    }

    fn direction_bits(&self) -> &[bool] {
        &self.direction
    }

    fn extend(&mut self, other: &IoSequence) {
        self.io.extend_from_slice(other.io_bits());
        self.direction.extend_from_slice(other.direction_bits());
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum TransferType {
    Read,
    Write(u32),
}

fn build_swd_transfer(port: PortType, direction: TransferType, address: u8) -> IoSequence {
    // JLink operates on raw SWD bit sequences.
    // So we need to manually assemble the read and write bitsequences.
    // The following code with the comments hopefully explains well enough how it works.
    // `true` means `1` and `false` means `0` for the SWDIO sequence.
    // `true` means `drive line` and `false` means `open drain` for the direction sequence.

    // First we determine the APnDP bit.
    let port = match port {
        PortType::DebugPort => false,
        PortType::AccessPort => true,
    };

    // Set direction bit to 1 for reads.
    let direction_bit = direction == TransferType::Read;

    // Then we determine the address bits.
    // Only bits 2 and 3 are relevant as we use byte addressing but can only read 32bits
    // which means we can skip bits 0 and 1. The ADI specification is defined like this.
    let a2 = (address >> 2) & 0x01 == 1;
    let a3 = (address >> 3) & 0x01 == 1;

    let mut sequence = IoSequence::new();

    // First we make sure we have the SDWIO line on idle for at least 2 clock cylces.
    sequence.add_output(false);
    sequence.add_output(false);

    // Then we assemble the actual request.

    // Start bit (always 1).
    sequence.add_output(true);

    // APnDP (0 for DP, 1 for AP).
    sequence.add_output(port);

    // RnW (0 for Write, 1 for Read).
    sequence.add_output(direction_bit);

    // Address bits
    sequence.add_output(a2);
    sequence.add_output(a3);

    // Odd parity bit over APnDP, RnW a2 and a3
    sequence.add_output(port ^ direction_bit ^ a2 ^ a3);

    // Stop bit (always 0).
    sequence.add_output(false);

    // Park bit (always 1).
    sequence.add_output(true);

    // Turnaround bit.
    sequence.add_input();

    // ACK bits.
    sequence.add_input_sequence(3);

    if let TransferType::Write(mut value) = direction {
        // For writes, we need to add two turnaround bits.
        // Theoretically the spec says that there is only one turnaround bit required here, where no clock is driven.
        // This seems to not be the case in actual implementations. So we insert two turnaround bits here!
        sequence.add_input();

        // Now we add all the data bits to the sequence and in the same loop we also calculate the parity bit.
        let mut parity = false;
        for _ in 0..32 {
            let bit = value & 1 == 1;
            sequence.add_output(bit);
            parity ^= bit;
            value >>= 1;
        }

        sequence.add_output(parity);
    } else {
        // Handle Read
        // Add the data bits to the SWDIO sequence.
        sequence.add_input_sequence(32);

        // Add the parity bit to the sequence.
        sequence.add_input();

        // Finally add the turnaround bit to the sequence.
        sequence.add_input();
    }

    sequence
}

fn response_length(direction: TransferDirection) -> usize {
    match direction {
        TransferDirection::Read => 2 + 8 + 3 + 32 + 1 + 2,
        TransferDirection::Write => 2 + 8 + 3 + 2 + 32 + 1,
    }
}

fn parse_swd_response(response: &[bool], direction: TransferDirection) -> Result<u32, DapError> {
    // We need to discard the output bits that correspond to the part of the request
    // in which the probe is driving SWDIO. Additionally, there is a phase shift that
    // happens when ownership of the SWDIO line is transfered to the device.
    // The device changes the value of SWDIO with the rising edge of the clock.
    //
    // It appears that the JLink probe samples this line with the falling edge of
    // the clock. Therefore, the whole sequence seems to be leading by one bit,
    // which is why we don't discard the turnaround bit. It actually contains the
    // first ack bit.

    // There are two idle bits and eight request bits,
    // the acknowledge comes directly after.
    let ack_offset = 2 + 8;

    // Get the ack.
    let ack = &response[ack_offset..ack_offset + 3];

    let read_value_offset = ack_offset + 3;

    let register_val: Vec<bool> = (&response[read_value_offset..read_value_offset + 32]).to_owned();

    let parity_bit = response[read_value_offset + 32];

    // When all bits are high, this means we didn't get any response from the
    // target, which indicates a protocol error.
    if ack[0] && ack[1] && ack[2] {
        return Err(DapError::NoAcknowledge);
    }
    if ack[1] {
        return Err(DapError::WaitResponse);
    }
    if ack[2] {
        return Err(DapError::FaultResponse);
    }

    if ack[0] {
        // Extract value, if it is a read

        if let TransferDirection::Read = direction {
            // Take the data bits and convert them into a 32bit int.
            let value = bits_to_byte(register_val);

            // Make sure the parity is correct.
            if (value.count_ones() % 2 == 1) == parity_bit {
                log::trace!("DAP read {}.", value);
                Ok(value)
            } else {
                Err(DapError::IncorrectParity)
            }
        } else {
            // Write, don't parse response
            Ok(0)
        }
    } else {
        // Invalid response
        log::debug!(
            "Unexpected response from target, does not conform to SWD specfication (ack={:?})",
            ack
        );
        Err(DapError::SwdProtocol)
    }
}

pub trait RawSwdIo {
    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>;

    fn swd_settings(&self) -> &SwdSettings;

    fn swd_statistics(&mut self) -> &mut SwdStatistics;

    /// Try to perform a SWD line reset, followed by a read of the DPIDR register.
    ///
    /// Returns Ok if the read of the DPIDR register was succesful, and Err
    /// otherwise. In case of JLink Errors, the actual error is returned.
    ///
    /// If the first line reset fails, it is tried once again, as the target
    /// might be in the middle of a transfer the first time we try the reset.
    ///
    /// See section B4.3.3 in the ADIv5 Specification.
    fn swd_line_reset(&mut self) -> Result<(), DebugProbeError>;
}

impl RawSwdIo for JLink {
    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>,
    {
        self.swd_statistics.report_io();

        let iter = self.handle.swd_io(dir, swdio)?;

        Ok(iter.collect())
    }

    fn swd_line_reset(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Performing line reset!");

        const NUM_RESET_BITS: usize = 50;

        let mut io_sequence = IoSequence::new();

        io_sequence.add_output_sequence(&[true; NUM_RESET_BITS]);

        io_sequence.extend(&build_swd_transfer(
            PortType::DebugPort,
            TransferType::Read,
            0,
        ));

        let mut result = Ok(());

        for _ in 0..2 {
            self.swd_statistics().report_line_reset();

            let result_sequence = self.swd_io(
                io_sequence.direction_bits().to_owned(),
                io_sequence.io_bits().to_owned(),
            )?;

            // Parse the response after the reset bits.
            match parse_swd_response(&result_sequence[NUM_RESET_BITS..], TransferDirection::Read) {
                Ok(_) => {
                    // Line reset was succesful
                    return Ok(());
                }
                Err(e) => {
                    // Try again, first reset might fail.
                    result = Err(e);
                }
            }
        }

        // No acknowledge from the target, even if after line reset
        result.map_err(|e| e.into())
    }

    fn swd_settings(&self) -> &SwdSettings {
        &self.swd_settings
    }

    fn swd_statistics(&mut self) -> &mut SwdStatistics {
        &mut self.swd_statistics
    }
}

impl<Probe: RawSwdIo + 'static> RawDapAccess for Probe {
    fn select_dp(&mut self, dp: DpAddress) -> Result<(), DebugProbeError> {
        match dp {
            DpAddress::Default => Ok(()), // nop
            DpAddress::Multidrop(_) => Err(DebugProbeError::ProbeSpecific(
                anyhow::anyhow!("JLink doesn't support multidrop SWD yet").into(),
            )),
        }
    }

    fn raw_read_register(&mut self, port: PortType, address: u8) -> Result<u32, DebugProbeError> {
        let dap_wait_retries = self.swd_settings().num_retries_after_wait;
        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        // Now we try to issue the request until it fails or succeeds.
        // If we timeout we retry a maximum of 5 times.
        for retry in 0..dap_wait_retries {
            let mut transfers = [SwdTransfer::read(port, address)];

            perform_transfers(self, &mut transfers, idle_cycles)?;

            match transfers[0].status {
                TransferStatus::Ok => {
                    return Ok(transfers[0].value);
                }
                TransferStatus::Pending => {
                    panic!("Unexpected transfer state after reading register. This is a bug!");
                }
                TransferStatus::Failed(DapError::WaitResponse) => {
                    // If ack[1] is set the host must retry the request. So let's do that right away!
                    log::debug!(
                        "DAP WAIT, (read), retries remaining {}.",
                        dap_wait_retries - retry
                    );

                    // Because we use overrun detection, we now have to clear the overrun error
                    let mut abort = Abort(0);

                    abort.set_orunerrclr(true);

                    RawDapAccess::raw_write_register(
                        self,
                        PortType::DebugPort,
                        Abort::ADDRESS,
                        abort.into(),
                    )?;

                    log::debug!("Cleared sticky overrun bit");

                    idle_cycles = std::cmp::min(
                        self.swd_settings().max_retry_idle_cycles_after_wait,
                        idle_cycles * 2,
                    );

                    continue;
                }
                TransferStatus::Failed(DapError::FaultResponse) => {
                    log::debug!("DAP FAULT");

                    // A fault happened during operation.

                    // To get a clue about the actual fault we read the ctrl register,
                    // which will have the fault status flags set.
                    let response =
                        RawDapAccess::raw_read_register(self, PortType::DebugPort, Ctrl::ADDRESS)?;
                    let ctrl = Ctrl::from(response);
                    log::debug!(
                        "Reading DAP register failed. Ctrl/Stat register value is: {:#?}",
                        ctrl
                    );

                    // Check the reason for the fault
                    // Other fault reasons than overrun or write error are not handled yet.
                    if ctrl.sticky_orun() || ctrl.sticky_err() {
                        // We did not handle a WAIT state properly

                        // Because we use overrun detection, we now have to clear the overrun error
                        let mut abort = Abort(0);

                        // Clear sticky error flags
                        abort.set_orunerrclr(ctrl.sticky_orun());
                        abort.set_stkerrclr(ctrl.sticky_err());

                        RawDapAccess::raw_write_register(
                            self,
                            PortType::DebugPort,
                            Abort::ADDRESS,
                            abort.into(),
                        )?;
                    }

                    return Err(DapError::FaultResponse.into());
                }
                // The other errors mean that something went wrong with the protocol itself,
                // so we try to perform a line reset, and recover.
                TransferStatus::Failed(_) => {
                    log::debug!("DAP NACK");

                    // Because we clock the SWDCLK line after receving the WAIT response,
                    // the target might be in weird state. If we perform a line reset,
                    // we should be able to recover from this.
                    self.swd_line_reset()?;

                    // Retry operation again
                    continue;
                }
            }
        }

        // If we land here, the DAP operation timed out.
        log::error!("DAP read timeout.");
        Err(DebugProbeError::Timeout)
    }

    fn raw_read_block(
        &mut self,
        port: PortType,
        address: u8,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        let mut succesful_transfers = 0;

        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        'transfer: for _ in 0..self.swd_settings().num_retries_after_wait {
            if succesful_transfers == values.len() {
                break;
            }

            let mut transfers =
                vec![SwdTransfer::read(port, address); values.len() - succesful_transfers];

            perform_transfers(self, &mut transfers, idle_cycles)?;

            let index_offset = succesful_transfers;

            for (index, result) in transfers.iter().enumerate() {
                match &result.status {
                    TransferStatus::Ok => {
                        values[index_offset + index] = result.value;
                        succesful_transfers += 1;
                    }
                    TransferStatus::Failed(err) => {
                        log::debug!(
                            "Error in access {}/{} of block access: {}",
                            index + 1,
                            values.len(),
                            err
                        );

                        if err == &DapError::WaitResponse {
                            // Clear STICKORRUN flag.

                            // Because we use overrun detection, we now have to clear the overrun error.
                            let mut abort = Abort(0);

                            abort.set_orunerrclr(true);

                            RawDapAccess::raw_write_register(
                                self,
                                PortType::DebugPort,
                                Abort::ADDRESS,
                                abort.into(),
                            )?;

                            idle_cycles = std::cmp::min(
                                self.swd_settings().max_retry_idle_cycles_after_wait,
                                idle_cycles * 2,
                            );

                            log::debug!("Retrying access {}", index_offset + index + 1);

                            continue 'transfer;
                        }
                        return Err(err.clone().into());
                    }
                    TransferStatus::Pending => {
                        // This should not happen...
                        panic!("Error performing transfers. This is a bug, please report it.")
                    }
                }
            }
        }

        Ok(())
    }

    fn raw_write_register(
        &mut self,
        port: PortType,
        address: u8,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        let dap_wait_retries = self.swd_settings().num_retries_after_wait;
        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        // Now we try to issue the request until it fails or succeeds.
        // If we timeout we retry a maximum of 5 times.
        for retry in 0..dap_wait_retries {
            let mut transfers = [SwdTransfer::write(port, address, value)];

            perform_transfers(self, &mut transfers, idle_cycles)?;

            match transfers[0].status {
                TransferStatus::Ok => {
                    return Ok(());
                }
                TransferStatus::Pending => {
                    panic!("Unexpected transfer state after writing register. This is a bug!");
                }
                TransferStatus::Failed(DapError::WaitResponse) => {
                    // If ack[1] is set the host must retry the request. So let's do that right away!
                    log::debug!(
                        "DAP WAIT, (write), retries remaining {}.",
                        dap_wait_retries - retry
                    );

                    let mut abort = Abort(0);

                    abort.set_orunerrclr(true);

                    // Because we use overrun detection, we now have to clear the overrun error
                    RawDapAccess::raw_write_register(
                        self,
                        PortType::DebugPort,
                        Abort::ADDRESS,
                        abort.into(),
                    )?;

                    log::debug!("Cleared sticky overrun bit");

                    idle_cycles = std::cmp::min(
                        self.swd_settings().max_retry_idle_cycles_after_wait,
                        idle_cycles * 2,
                    );

                    continue;
                }
                TransferStatus::Failed(DapError::FaultResponse) => {
                    log::debug!("DAP FAULT");
                    // A fault happened during operation.

                    // To get a clue about the actual fault we read the ctrl register,
                    // which will have the fault status flags set.

                    let response =
                        RawDapAccess::raw_read_register(self, PortType::DebugPort, Ctrl::ADDRESS)?;

                    let ctrl = Ctrl::from(response);
                    log::trace!(
                        "Writing DAP register failed. Ctrl/Stat register value is: {:#?}",
                        ctrl
                    );

                    // Check the reason for the fault
                    // Other fault reasons than overrun or write error are not handled yet.
                    if ctrl.sticky_orun() || ctrl.sticky_err() {
                        // We did not handle a WAIT state properly

                        // Because we use overrun detection, we now have to clear the overrun error
                        let mut abort = Abort(0);

                        // Clear sticky error flags
                        abort.set_orunerrclr(ctrl.sticky_orun());
                        abort.set_stkerrclr(ctrl.sticky_err());

                        RawDapAccess::raw_write_register(
                            self,
                            PortType::DebugPort,
                            Abort::ADDRESS,
                            abort.into(),
                        )?;
                    }

                    return Err(DapError::FaultResponse.into());
                }
                // The other errors mean that something went wrong with the protocol itself,
                // so we try to perform a line reset, and recover.
                TransferStatus::Failed(_) => {
                    log::debug!("DAP NACK");

                    // Because we clock the SWDCLK line after receving the WAIT response,
                    // the target might be in weird state. If we perform a line reset,
                    // we should be able to recover from this.
                    self.swd_line_reset()?;

                    // Retry operation
                    continue;
                }
            }
        }

        // If we land here, the DAP operation timed out.
        log::error!("DAP write timeout.");
        Err(DebugProbeError::Timeout)
    }

    fn raw_write_block(
        &mut self,
        port: PortType,
        address: u8,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        let mut succesful_transfers = 0;

        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        'transfer: for _ in 0..self.swd_settings().num_retries_after_wait {
            if succesful_transfers == values.len() {
                break;
            }

            let mut transfers: Vec<SwdTransfer> = values
                .iter()
                .skip(succesful_transfers)
                .map(|v| SwdTransfer::write(port, address, *v))
                .collect();

            let index_offset = succesful_transfers;

            perform_transfers(self, &mut transfers, idle_cycles)?;

            for (index, result) in transfers.iter().enumerate() {
                match &result.status {
                    TransferStatus::Ok => {
                        succesful_transfers += 1;
                    }
                    TransferStatus::Failed(err) => {
                        log::debug!(
                            "Error in access {}/{} of block access: {}",
                            index_offset + index + 1,
                            values.len(),
                            err
                        );

                        if err == &DapError::WaitResponse {
                            // Clear STICKORRUN flag.

                            // Because we use overrun detection, we now have to clear the overrun error.
                            let mut abort = Abort(0);

                            abort.set_orunerrclr(true);

                            RawDapAccess::raw_write_register(
                                self,
                                PortType::DebugPort,
                                Abort::ADDRESS,
                                abort.into(),
                            )?;

                            idle_cycles = std::cmp::min(
                                self.swd_settings().max_retry_idle_cycles_after_wait,
                                idle_cycles * 2,
                            );

                            log::debug!("Retrying access {}", index_offset + index + 1);

                            continue 'transfer;
                        }

                        return Err(err.clone().into());
                    }
                    TransferStatus::Pending => {
                        // This should not happen...
                        panic!("Error performing transfers. This is a bug, please report it.")
                    }
                }
            }

            return Ok(());
        }

        Ok(())
    }
}

#[cfg(test)]
mod test {

    use std::iter;

    use crate::architecture::arm::{PortType, RawDapAccess};

    use super::{RawSwdIo, SwdSettings, SwdStatistics};

    use bitvec::prelude::*;

    #[allow(dead_code)]
    enum DapAcknowledge {
        Ok,
        Wait,
        Fault,
        NoAck,
    }

    struct MockJaylink {
        direction_input: Option<Vec<bool>>,
        io_input: Option<Vec<bool>>,
        transfer_responses: Vec<Vec<bool>>,

        expected_transfer_count: usize,
        performed_transfer_count: usize,

        swd_settings: SwdSettings,
        swd_statistics: SwdStatistics,
    }

    impl MockJaylink {
        fn new() -> Self {
            Self {
                direction_input: None,
                io_input: None,
                transfer_responses: vec![vec![]],

                expected_transfer_count: 1,
                performed_transfer_count: 0,

                swd_settings: SwdSettings::default(),
                swd_statistics: SwdStatistics::default(),
            }
        }

        fn add_write_response(&mut self, acknowledge: DapAcknowledge, idle_cycles: usize) {
            let last_transfer = self.transfer_responses.last_mut().unwrap();

            // The write consists of the following parts:
            //
            // - 2 idle bits
            // - 8 request bits
            // - 1 turnaround bit
            // - 3 acknowledge bits
            // - 2 turnaround bits
            // - x idle cycles
            let write_length = 2 + 8 + 1 + 3 + 2 + 32 + idle_cycles;

            let mut response = BitVec::<Lsb0, usize>::repeat(false, write_length);

            match acknowledge {
                DapAcknowledge::Ok => {
                    // Set acknowledege to OK
                    response.set(10, true);
                }
                DapAcknowledge::Wait => {
                    // Set acknowledege to WAIT
                    response.set(11, true);
                }
                DapAcknowledge::Fault => {
                    // Set acknowledege to FAULT
                    response.set(12, true);
                }
                DapAcknowledge::NoAck => {
                    // No acknowledge means that all acknowledge bits
                    // are set to false.
                }
            }

            last_transfer.extend(response);
        }

        fn add_read_response(&mut self, acknowledge: DapAcknowledge, value: u32) {
            let last_transfer = self.transfer_responses.last_mut().unwrap();

            // The read consists of the following parts:
            //
            // - 2 idle bits
            // - 8 request bits
            // - 1 turnaround bit
            // - 3 acknowledge bits
            // - 2 turnaround bits
            let write_length = 2 + 8 + 1 + 3 + 32 + 2;

            let mut response = BitVec::<Lsb0, usize>::repeat(false, write_length);

            match acknowledge {
                DapAcknowledge::Ok => {
                    // Set acknowledege to OK
                    response.set(10, true);
                }
                DapAcknowledge::Wait => {
                    // Set acknowledege to WAIT
                    response.set(11, true);
                }
                DapAcknowledge::Fault => {
                    // Set acknowledege to FAULT
                    response.set(12, true);
                }
                DapAcknowledge::NoAck => {
                    // No acknowledge means that all acknowledge bits
                    // are set to false.
                }
            }

            // Set the read value
            response.get_mut(13..13 + 32).unwrap().store_le(value);

            // calculate the parity bit
            let parity_bit = value.count_ones() % 2 == 1;
            response.set(13 + 32, parity_bit);

            last_transfer.extend(response);
        }

        fn add_idle_cycles(&mut self, len: usize) {
            let last_transfer = self.transfer_responses.last_mut().unwrap();

            last_transfer.extend(iter::repeat(false).take(len))
        }

        fn add_transfer(&mut self) {
            self.transfer_responses.push(Vec::new());
            self.expected_transfer_count += 1;
        }
    }

    impl RawSwdIo for MockJaylink {
        fn swd_io<'a, D, S>(
            &'a mut self,
            dir: D,
            swdio: S,
        ) -> Result<Vec<bool>, crate::DebugProbeError>
        where
            D: IntoIterator<Item = bool>,
            S: IntoIterator<Item = bool>,
        {
            self.direction_input = Some(dir.into_iter().collect());
            self.io_input = Some(swdio.into_iter().collect());

            assert_eq!(
                self.direction_input.as_ref().unwrap().len(),
                self.io_input.as_ref().unwrap().len()
            );

            let transfer_response = self.transfer_responses.remove(0);

            assert_eq!(
                transfer_response.len(),
                self.io_input.as_ref().map(|v| v.len()).unwrap(),
                "Length mismatch for transfer {}/{}",
                self.performed_transfer_count + 1,
                self.expected_transfer_count
            );

            self.performed_transfer_count += 1;

            Ok(transfer_response)
        }

        fn swd_line_reset(&mut self) -> Result<(), crate::DebugProbeError> {
            Ok(())
        }

        fn swd_settings(&self) -> &SwdSettings {
            &self.swd_settings
        }

        fn swd_statistics(&mut self) -> &mut SwdStatistics {
            &mut self.swd_statistics
        }
    }

    #[test]
    fn read_register() {
        let read_value = 12;
        let mut mock = MockJaylink::new();

        mock.add_read_response(DapAcknowledge::Ok, 0);
        mock.add_read_response(DapAcknowledge::Ok, read_value);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        let result = mock.raw_read_register(PortType::AccessPort, 4).unwrap();

        assert_eq!(result, read_value);
    }

    #[test]
    fn read_register_with_wait_response() {
        let read_value = 47;
        let mut mock = MockJaylink::new();

        mock.add_read_response(DapAcknowledge::Ok, 0);
        mock.add_read_response(DapAcknowledge::Wait, 0);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        //  When a wait response is received, the sticky overrun bit has to be cleared

        mock.add_transfer();
        mock.add_write_response(
            DapAcknowledge::Ok,
            mock.swd_settings.num_idle_cycles_between_writes,
        );
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        mock.add_transfer();
        mock.add_read_response(DapAcknowledge::Ok, 0);
        mock.add_read_response(DapAcknowledge::Ok, read_value);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        let result = mock.raw_read_register(PortType::AccessPort, 4).unwrap();

        assert_eq!(result, read_value);
    }

    #[test]
    fn write_register() {
        let mut mock = MockJaylink::new();

        let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

        mock.add_write_response(DapAcknowledge::Ok, idle_cycles);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_before_write_verify);
        mock.add_read_response(DapAcknowledge::Ok, 0);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        mock.raw_write_register(PortType::AccessPort, 4, 0x123)
            .expect("Failed to write register");
    }

    #[test]
    fn write_register_with_wait_response() {
        let mut mock = MockJaylink::new();
        let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

        mock.add_write_response(DapAcknowledge::Ok, idle_cycles);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_before_write_verify);
        mock.add_read_response(DapAcknowledge::Wait, 0);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        // Expect a Write to the ABORT register.
        mock.add_transfer();
        mock.add_write_response(DapAcknowledge::Ok, idle_cycles);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        // Second try to write register, with increased idle cycles.
        mock.add_transfer();
        mock.add_write_response(DapAcknowledge::Ok, idle_cycles * 2);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_before_write_verify);
        mock.add_read_response(DapAcknowledge::Ok, 0);
        mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

        mock.raw_write_register(PortType::AccessPort, 4, 0x123)
            .expect("Failed to write register");
    }

    /// Test the correct handling of several transfers, with
    /// the appropriate extra reads added as necessary.
    mod transfer_handling {
        use crate::{
            architecture::arm::PortType,
            probe::jlink::swd::{perform_transfers, SwdTransfer, TransferStatus},
        };

        use super::{DapAcknowledge, MockJaylink};

        #[test]
        fn single_dp_register_read() {
            let register_value = 32354;

            let mut transfers = vec![SwdTransfer::read(PortType::DebugPort, 0)];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, register_value);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            let transfer_result = &transfers[0];

            assert_eq!(transfer_result.status, TransferStatus::Ok);
            assert_eq!(transfer_result.value, register_value);
        }

        #[test]
        fn single_ap_register_read() {
            let register_value = 0x11_22_33_44u32;

            let mut transfers = vec![SwdTransfer::read(PortType::AccessPort, 0)];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_read_response(DapAcknowledge::Ok, register_value);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            let transfer_result = &transfers[0];

            assert_eq!(transfer_result.status, TransferStatus::Ok);
            assert_eq!(transfer_result.value, register_value);
        }

        #[test]
        fn ap_then_dp_register_read() {
            // When reading from the AP first, and then from the DP,
            // we need to insert an additional read from the RDBUFF register to
            // get the result for the AP read.

            let ap_read_value = 0x123223;
            let dp_read_value = 0xFFAABB;

            let mut transfers = vec![
                SwdTransfer::read(PortType::AccessPort, 4),
                SwdTransfer::read(PortType::DebugPort, 3),
            ];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_read_response(DapAcknowledge::Ok, ap_read_value);
            mock.add_read_response(DapAcknowledge::Ok, dp_read_value);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            assert_eq!(transfers[0].status, TransferStatus::Ok);
            assert_eq!(transfers[0].value, ap_read_value);

            assert_eq!(transfers[1].status, TransferStatus::Ok);
            assert_eq!(transfers[1].value, dp_read_value);
        }

        #[test]
        fn dp_then_ap_register_read() {
            // When reading from the DP first, and then from the AP,
            // we need to insert an additional read from the RDBUFF register at the end
            // to get the result for the AP read.

            let ap_read_value = 0x123223;
            let dp_read_value = 0xFFAABB;

            let mut transfers = vec![
                SwdTransfer::read(PortType::DebugPort, 3),
                SwdTransfer::read(PortType::AccessPort, 4),
            ];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, dp_read_value);
            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_read_response(DapAcknowledge::Ok, ap_read_value);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            assert_eq!(transfers[0].status, TransferStatus::Ok);
            assert_eq!(transfers[0].value, dp_read_value);

            assert_eq!(transfers[1].status, TransferStatus::Ok);
            assert_eq!(transfers[1].value, ap_read_value);
        }

        #[test]
        fn multiple_ap_read() {
            // When reading from the AP twice, only a single additional read from the
            // RDBUFF register is necessary.

            let ap_read_values = [1, 2];

            let mut transfers = vec![
                SwdTransfer::read(PortType::AccessPort, 4),
                SwdTransfer::read(PortType::AccessPort, 4),
            ];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_read_response(DapAcknowledge::Ok, ap_read_values[0]);
            mock.add_read_response(DapAcknowledge::Ok, ap_read_values[1]);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            assert_eq!(transfers[0].status, TransferStatus::Ok);
            assert_eq!(transfers[0].value, ap_read_values[0]);

            assert_eq!(transfers[1].status, TransferStatus::Ok);
            assert_eq!(transfers[1].value, ap_read_values[1]);
        }

        #[test]
        fn multiple_dp_read() {
            // When reading from the DP twice, no additional reads have to be inserted.

            let dp_read_values = [1, 2];

            let mut transfers = vec![
                SwdTransfer::read(PortType::DebugPort, 4),
                SwdTransfer::read(PortType::DebugPort, 4),
            ];

            let mut mock = MockJaylink::new();

            mock.add_read_response(DapAcknowledge::Ok, dp_read_values[0]);
            mock.add_read_response(DapAcknowledge::Ok, dp_read_values[1]);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, 16).expect("Failed to perform transfer");

            assert_eq!(transfers[0].status, TransferStatus::Ok);
            assert_eq!(transfers[0].value, dp_read_values[0]);

            assert_eq!(transfers[1].status, TransferStatus::Ok);
            assert_eq!(transfers[1].value, dp_read_values[1]);
        }

        #[test]
        fn single_dp_register_write() {
            let mut transfers = vec![SwdTransfer::write(PortType::DebugPort, 0, 0x1234_5678)];

            let mut mock = MockJaylink::new();
            let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );

            // To verify that the write was succesfull, an additional read is performed.
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, idle_cycles)
                .expect("Failed to perform transfer");

            let transfer_result = &transfers[0];

            assert_eq!(transfer_result.status, TransferStatus::Ok);
        }

        #[test]
        fn single_ap_register_write() {
            let mut transfers = vec![SwdTransfer::write(PortType::AccessPort, 0, 0x1234_5678)];

            let mut mock = MockJaylink::new();

            let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );

            // To verify that the write was succesfull, an additional read is performed.
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_before_write_verify);
            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, idle_cycles)
                .expect("Failed to perform transfer");

            let transfer_result = &transfers[0];

            assert_eq!(transfer_result.status, TransferStatus::Ok);
        }

        #[test]
        fn multiple_ap_register_write() {
            let mut transfers = vec![
                SwdTransfer::write(PortType::AccessPort, 0, 0x1234_5678),
                SwdTransfer::write(PortType::AccessPort, 0, 0xABABABAB),
            ];

            let mut mock = MockJaylink::new();

            let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );
            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );

            mock.add_idle_cycles(mock.swd_settings.idle_cycles_before_write_verify);
            mock.add_read_response(DapAcknowledge::Ok, 0);
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, idle_cycles)
                .expect("Failed to perform transfer");

            assert_eq!(transfers[0].status, TransferStatus::Ok);
            assert_eq!(transfers[1].status, TransferStatus::Ok);
        }
    }
}
