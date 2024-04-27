//! Implementation of the ARM Debug Interface
//!
//! This module implements functions to work with chips implementing the ARM Debug version v5.
//!
//! See <https://developer.arm.com/documentation/ihi0031/f/?lang=en> for the ADIv5 specification.

use crate::{
    architecture::arm::{
        dp::{Abort, Ctrl, RdBuff, DPIDR},
        ArmError, DapError, PortType, RawDapAccess, Register,
    },
    probe::{common::bits_to_byte, DebugProbe, DebugProbeError, JTAGAccess, WireProtocol},
};

#[derive(Debug)]
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
    ///
    /// When performing a write operation, the write can
    /// be buffered, meaning that completing the transfer
    /// does not mean that the write was performed successfully.
    ///
    /// To check that all writes have been executed, the
    /// `RDBUFF` register can be read from the DP.
    ///
    /// If any writes are still pending, this read will result in a WAIT response.
    /// By adding idle cycles before performing this read, the chance of a
    /// WAIT response is smaller.
    pub idle_cycles_before_write_verify: usize,

    /// Number of idle cycles to insert after a transfer
    ///
    /// It is recommended that at least 8 idle cycles are
    /// inserted.
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

#[derive(Default, Debug)]
pub struct ProbeStatistics {
    /// Number of protocol transfers performed.
    ///
    /// This includes repeated transfers, and transfers
    /// which are automatically added to fulfill
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

impl ProbeStatistics {
    pub fn record_extra_transfer(&mut self) {
        self.num_extra_transfers += 1;
    }

    pub fn record_transfers(&mut self, num_transfers: usize) {
        self.num_transfers += num_transfers;
    }

    pub fn report_io(&mut self) {
        self.num_io_calls += 1;
    }

    pub fn report_swd_response<T>(&mut self, response: &Result<T, DapError>) {
        match response {
            Err(DapError::FaultResponse) => self.num_faults += 1,
            Err(DapError::WaitResponse) => self.num_wait_resp += 1,
            // Other errors are not counted right now.
            _ => (),
        }
    }

    pub fn report_line_reset(&mut self) {
        self.num_line_resets += 1;
    }
}

// Constant to be written to ABORT
const JTAG_ABORT_VALUE: u64 = 0x8;

// IR values for JTAG registers
const JTAG_ABORT_IR_VALUE: u32 = 0x8;
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
        let address = match transfer.port {
            PortType::DebugPort => JTAG_DEBUG_PORT_IR_VALUE,
            PortType::AccessPort => JTAG_ACCESS_PORT_IR_VALUE,
        };

        let mut payload = 0u64;

        // 32-bit value, bits 35:3
        payload |= (transfer.value as u64) << 3;
        // A[3:2], bits 2:1
        payload |= (transfer.address as u64 & 0b1000) >> 1;
        payload |= (transfer.address as u64 & 0b0100) >> 1;
        // RnW, bit 0
        payload |= u64::from(transfer.direction == TransferDirection::Read);

        (payload, address)
    }
}

fn parse_jtag_response(data: &[u8]) -> u64 {
    let mut received = 0u64;
    for v in data.iter() {
        received >>= 8;
        received |= (*v as u64) << 32;
    }

    received
}

/// Perform a single JTAG transfer and parse the results
///
/// Return is (value, status)
fn perform_jtag_transfer<P: JTAGAccess + RawProtocolIo>(
    probe: &mut P,
    transfer: &DapTransfer,
) -> Result<(u32, TransferStatus), DebugProbeError> {
    // Determine what JTAG IR address and value to send
    let (payload, address) = build_jtag_payload_and_address(transfer);
    let data = payload.to_le_bytes();

    let idle_cycles = probe.idle_cycles();
    probe.set_idle_cycles(transfer.idle_cycles_after.min(255) as u8);

    // This is a bit confusing, but a read from any port is still
    // a JTAG write as we have to transmit the address
    let result = probe.write_register(address, &data[..], JTAG_DR_BIT_LENGTH);

    probe.set_idle_cycles(idle_cycles);

    let result = result?;

    let received = parse_jtag_response(&result);

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
            tracing::error!("Unexpected DAP response: {}", status);

            TransferStatus::Failed(DapError::NoAcknowledge)
        }
    };

    Ok((received_value, transfer_status))
}

/// Perform a batch of JTAG transfers.
///
/// Each transfer is sent one at a time using the JTAGAccess trait
fn perform_jtag_transfers<P: JTAGAccess + RawProtocolIo>(
    probe: &mut P,
    transfers: &mut [DapTransfer],
) -> Result<(), DebugProbeError> {
    for i in 0..transfers.len() {
        // Send payload
        let (received_value, status) = perform_jtag_transfer(probe, &transfers[i])?;

        // Each response is read in the next transaction
        if i > 0 {
            let previous_transfer = &mut transfers[i - 1];
            previous_transfer.status =
                if previous_transfer.is_abort() || previous_transfer.is_rdbuff() {
                    // No status
                    TransferStatus::Ok
                } else {
                    if status == TransferStatus::Ok
                        && previous_transfer.direction == TransferDirection::Read
                    {
                        previous_transfer.value = received_value;
                    }
                    status
                };
        }
    }

    // We need to do a final read to get the status for the last transaction
    let last_transfer = &mut transfers[transfers.len() - 1];
    if last_transfer.is_abort() || last_transfer.is_rdbuff() {
        // No acknowledgement, so need need for another transfer
        last_transfer.status = TransferStatus::Ok;
    } else {
        // Need to issue a fake read to get final ack
        let rdbuff_transfer = DapTransfer::read(PortType::DebugPort, RdBuff::ADDRESS);

        let (received_value, status) = perform_jtag_transfer(probe, &rdbuff_transfer)?;

        last_transfer.status = status;
        if last_transfer.status == TransferStatus::Ok
            && last_transfer.direction == TransferDirection::Read
        {
            last_transfer.value = received_value;
        }
    }

    if !last_transfer.is_abort() {
        // Check CTRL/STATUS to make sure OK/FAULT meant OK
        let (_, _) = perform_jtag_transfer(
            probe,
            &DapTransfer::read(PortType::DebugPort, Ctrl::ADDRESS),
        )?;
        let (received_value, _) = perform_jtag_transfer(
            probe,
            &DapTransfer::read(PortType::DebugPort, RdBuff::ADDRESS),
        )?;

        if Ctrl(received_value).sticky_err() {
            tracing::debug!("JTAG transaction set failed: {:#X?}", transfers);

            // Clear the sticky bit so future transactions succeed
            let (_, _) = perform_jtag_transfer(
                probe,
                &DapTransfer::write(PortType::DebugPort, Ctrl::ADDRESS, received_value),
            )?;

            // Mark OK/FAULT transactions as failed
            // The caller will reset the sticky flag and retry if needed
            for transfer in transfers {
                if transfer.status == TransferStatus::Ok {
                    transfer.status = TransferStatus::Failed(DapError::FaultResponse);
                }
            }
        }
    }

    Ok(())
}

/// Perform a batch of SWD transfers.
///
/// For each transfer, the corresponding bit sequence is
/// created and the resulting sequences are concatenated
/// to a single sequence, so that it can be sent to
/// to the probe.
fn perform_swd_transfers<P: RawProtocolIo>(
    probe: &mut P,
    transfers: &mut [DapTransfer],
) -> Result<(), DebugProbeError> {
    let mut io_sequence = IoSequence::new();

    for transfer in transfers.iter() {
        io_sequence.extend(&transfer.io_sequence());
    }

    let result = probe.swd_io(io_sequence.direction_bits(), io_sequence.io_bits())?;

    let mut read_index = 0;

    for (i, transfer) in transfers.iter_mut().enumerate() {
        let response_direction = transfer.direction;
        let additional_idle_cycles_after = transfer.idle_cycles_after;

        let response = parse_swd_response(&result[read_index..], response_direction);

        probe.probe_statistics().report_swd_response(&response);

        tracing::debug!("Transfer result {}: {:x?}", i, response);

        transfer.status = match response {
            Ok(val) => {
                if transfer.direction == TransferDirection::Read {
                    transfer.value = val;
                }

                TransferStatus::Ok
            }
            Err(e) => TransferStatus::Failed(e),
        };

        read_index += response_length(response_direction);

        read_index += additional_idle_cycles_after;
    }

    Ok(())
}

/// Perform a batch of transfers.
///
/// Certain transfers require additional transfers to
/// get the result. This is handled by this function.
fn perform_transfers<P: DebugProbe + RawProtocolIo + JTAGAccess>(
    probe: &mut P,
    transfers: &mut [DapTransfer],
    idle_cycles: usize,
) -> Result<(), DebugProbeError> {
    assert!(!transfers.is_empty());

    // Read from DebugPort  -> Nothing special needed
    // Read from AccessPort -> Response is returned in next read
    //                         -> The next transfer must be a AP Read, otherwise we need to insert a read from the RDBUFF register
    // Write to any port    -> Status is reported in next transfer
    // Write to any port    -> Writes can be buffered, so certain transfers have to be avoided until a instruction which can be stalled is performed

    let mut final_transfers: Vec<DapTransfer> = Vec::with_capacity(transfers.len());

    struct OriginalTransfer {
        index: usize,
        response_in_next: bool,
    }
    let mut result_indices = Vec::with_capacity(transfers.len());

    let mut num_transfers = 0;

    let mut need_ap_read = false;
    let mut buffered_write = false;
    let mut write_response_pending = false;

    for transfer in transfers.iter() {
        // Check if we need to insert an additional read from the RDBUFF register
        if !transfer.is_ap_read() && need_ap_read {
            final_transfers.push(DapTransfer::read(PortType::DebugPort, RdBuff::ADDRESS));
            num_transfers += 1;

            // This is an extra transfer, which doesn't have a reponse on it's own.
            probe.probe_statistics().record_extra_transfer();
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
                if let Some(transfer) = final_transfers.last_mut() {
                    transfer.idle_cycles_after +=
                        probe.swd_settings().idle_cycles_before_write_verify;
                }

                // Add a read from RDBUFF, this access will stalled by the DebugPort if the write buffer
                // is not empty.
                final_transfers.push(DapTransfer::read(PortType::DebugPort, RdBuff::ADDRESS));

                num_transfers += 1;

                // This is an extra transfer, which doesn't have a reponse on it's own.
                probe.probe_statistics().record_extra_transfer();
            }
        }

        final_transfers.push(transfer.clone());

        // The response for an AP read is returned in the next response
        need_ap_read = transfer.is_ap_read();

        // Writes to the AP can be buffered
        //
        // TODO: Can DP writes be buffered as well?
        buffered_write =
            transfer.port == PortType::AccessPort && transfer.direction == TransferDirection::Write;

        // For all writes, except writes to the DP ABORT register, we need to perform another register to ensure that
        // we know if the write succeeded.
        write_response_pending = transfer.is_write() && !transfer.is_abort();

        // Track whether the response is returned in the next transfer.
        // SWD only, with JTAG we always get responses in a predictable fashion so it's
        // handled by perform_jtag_transfers
        result_indices.push(OriginalTransfer {
            index: num_transfers,
            response_in_next: probe.active_protocol().unwrap() == WireProtocol::Swd
                && (need_ap_read || write_response_pending),
        });

        if transfer.is_write() {
            tracing::trace!("Adding {} idle cycles after transfer!", idle_cycles);

            final_transfers.last_mut().unwrap().idle_cycles_after = idle_cycles;
        }

        num_transfers += 1;
    }

    if need_ap_read || write_response_pending {
        if write_response_pending {
            if let Some(transfer) = final_transfers.last_mut() {
                transfer.idle_cycles_after += probe.swd_settings().idle_cycles_before_write_verify;
            }
        }

        final_transfers.push(DapTransfer::read(PortType::DebugPort, RdBuff::ADDRESS));

        num_transfers += 1;
        probe.probe_statistics().record_extra_transfer();
    }

    // Add idle cycles at the end, to ensure transfer is performed
    if probe.swd_settings().idle_cycles_after_transfer > 0 {
        final_transfers.last_mut().unwrap().idle_cycles_after +=
            probe.swd_settings().idle_cycles_after_transfer;
    }

    tracing::debug!(
        "Performing {} transfers ({} additional transfers)",
        num_transfers,
        num_transfers - transfers.len()
    );

    probe.probe_statistics().record_transfers(num_transfers);

    match probe.active_protocol().unwrap() {
        WireProtocol::Swd => perform_swd_transfers(probe, &mut final_transfers[..])?,
        WireProtocol::Jtag => perform_jtag_transfers(probe, &mut final_transfers[..])?,
    }

    // Retrieve the results
    for (transfer, orig) in transfers.iter_mut().zip(result_indices) {
        // if the original transfer caused two transfers, return the first non-OK status.
        // This is important if the first fails with WAIT and the second with FAULT. We need to
        // return WAIT so that higher layers know they have to retry.
        transfer.status = final_transfers[orig.index].status;
        if orig.response_in_next && transfer.status == TransferStatus::Ok {
            transfer.status = final_transfers[orig.index + 1].status;
        }

        if transfer.direction == TransferDirection::Read {
            transfer.value = final_transfers[orig.index + orig.response_in_next as usize].value;
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct DapTransfer {
    port: PortType,
    direction: TransferDirection,
    address: u8,
    value: u32,
    status: TransferStatus,
    idle_cycles_after: usize,
}

impl DapTransfer {
    fn read(port: PortType, address: u8) -> DapTransfer {
        Self {
            port,
            address,
            direction: TransferDirection::Read,
            value: 0,
            status: TransferStatus::Pending,
            idle_cycles_after: 0,
        }
    }

    fn write(port: PortType, address: u8, value: u32) -> DapTransfer {
        Self {
            port,
            address,
            value,
            direction: TransferDirection::Write,
            status: TransferStatus::Pending,
            idle_cycles_after: 0,
        }
    }

    fn transfer_type(&self) -> TransferType {
        match self.direction {
            TransferDirection::Read => TransferType::Read,
            TransferDirection::Write => TransferType::Write(self.value),
        }
    }

    fn io_sequence(&self) -> IoSequence {
        let mut seq = build_swd_transfer(self.port, self.transfer_type(), self.address);

        for _ in 0..self.idle_cycles_after {
            seq.add_output(false);
        }

        seq
    }

    // Helper functions for combining transfers

    fn is_ap_read(&self) -> bool {
        self.port == PortType::AccessPort && self.direction == TransferDirection::Read
    }

    fn is_write(&self) -> bool {
        self.direction == TransferDirection::Write
    }

    fn is_abort(&self) -> bool {
        self.port == PortType::DebugPort
            && self.address == Abort::ADDRESS
            && self.direction == TransferDirection::Write
    }

    fn is_rdbuff(&self) -> bool {
        self.port == PortType::DebugPort
            && self.address == RdBuff::ADDRESS
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

    fn from_bytes(data: &[u8], mut bits: usize) -> Self {
        let mut this = Self::new();

        'outer: for byte in data {
            for i in 0..8 {
                this.add_output(byte & (1 << i) != 0);
                bits -= 1;
                if bits == 0 {
                    break 'outer;
                }
            }
        }

        this
    }

    fn add_output(&mut self, bit: bool) {
        self.io.push(bit);
        self.direction.push(Self::OUTPUT);
    }

    fn add_input(&mut self) {
        self.io.push(false);
        self.direction.push(Self::INPUT);
    }

    fn add_input_sequence(&mut self, length: usize) {
        for _ in 0..length {
            self.add_input();
        }
    }

    fn io_bits(&self) -> impl Iterator<Item = bool> + '_ {
        self.io.iter().copied()
    }

    fn direction_bits(&self) -> impl Iterator<Item = bool> + '_ {
        self.direction.iter().copied()
    }

    fn extend(&mut self, other: &IoSequence) {
        self.io.extend_from_slice(&other.io);
        self.direction.extend_from_slice(&other.direction);
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

    // When all bits are high, this means we didn't get any response from the
    // target, which indicates a protocol error.
    match (ack[0], ack[1], ack[2]) {
        (true, true, true) => return Err(DapError::NoAcknowledge),
        (_, true, _) => return Err(DapError::WaitResponse),
        (_, _, true) => return Err(DapError::FaultResponse),
        _ => (),
    }

    if ack[0] {
        // Extract value, if it is a read

        if let TransferDirection::Read = direction {
            let read_value_offset = ack_offset + 3;
            let register_val = response[read_value_offset..][..32].iter().copied();
            let parity_bit = response[read_value_offset + 32];

            // Take the data bits and convert them into a 32bit int.
            let value = bits_to_byte(register_val);

            // Make sure the parity is correct.
            if (value.count_ones() % 2 == 1) == parity_bit {
                tracing::trace!("DAP read {}.", value);
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
        tracing::debug!(
            "Unexpected response from target, does not conform to SWD specfication (ack={:?})",
            ack
        );
        Err(DapError::SwdProtocol)
    }
}

pub trait RawProtocolIo {
    fn jtag_shift_tms<M>(&mut self, tms: M, tdi: bool) -> Result<(), DebugProbeError>
    where
        M: IntoIterator<Item = bool>;

    fn jtag_shift_tdi<I>(&mut self, tms: bool, tdi: I) -> Result<(), DebugProbeError>
    where
        I: IntoIterator<Item = bool>;

    fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
    where
        D: IntoIterator<Item = bool>,
        S: IntoIterator<Item = bool>;

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError>;

    fn swd_settings(&self) -> &SwdSettings;

    fn probe_statistics(&mut self) -> &mut ProbeStatistics;
}

fn line_reset<P: RawProtocolIo + JTAGAccess + RawDapAccess>(this: &mut P) -> Result<(), ArmError> {
    tracing::debug!("Performing line reset!");

    const NUM_RESET_BITS: u8 = 50;

    let idle_cycles = std::cmp::max(1, this.swd_settings().num_idle_cycles_between_writes);

    let mut result = Ok(());

    for _ in 0..2 {
        this.probe_statistics().report_line_reset();

        this.swj_sequence(NUM_RESET_BITS, 0x7FFFFFFFFFFFF)?;

        // TODO: there are two unhandled implications:
        // - A line reset deselects the current multidrop target
        // - A line reset sets CTRL/STAT.STICKYORUN to 0b1
        // ^ both of these are handled in select_dp. We should reuse it, but carefully to avoid
        //   an endless loop.

        // Read DPIDR register
        //
        // The `raw_read_register` function cannot be called here, because that function can call `line_reset` again,
        // resulting in an endless loop.
        let mut transfers = [DapTransfer::read(PortType::DebugPort, DPIDR::ADDRESS)];

        perform_transfers(this, &mut transfers, idle_cycles)?;

        match transfers[0].status {
            TransferStatus::Ok => return Ok(()),
            TransferStatus::Pending => {
                tracing::debug!("Unexpected pending status in line reset.");
                // Transfer will be retried.
            }
            TransferStatus::Failed(e) => {
                tracing::debug!("Error reading DPIDR register after line reset: {e:?}");
                result = Err(ArmError::from(e));
            }
        }
    }

    // No acknowledge from the target, even if after line reset
    result
}

impl<Probe: DebugProbe + RawProtocolIo + JTAGAccess + 'static> RawDapAccess for Probe {
    fn raw_read_register(&mut self, port: PortType, address: u8) -> Result<u32, ArmError> {
        assert!(address < 0x10, "Invalid register address {:#x}", address);

        let dap_wait_retries = self.swd_settings().num_retries_after_wait;
        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        // Now we try to issue the request until it fails or succeeds.
        // If we timeout we retry a maximum of 5 times.
        for retry in 0..dap_wait_retries {
            let mut transfers = [DapTransfer::read(port, address)];

            perform_transfers(self, &mut transfers, idle_cycles)?;

            match transfers[0].status {
                TransferStatus::Ok => return Ok(transfers[0].value),
                TransferStatus::Pending => {
                    panic!("Unexpected transfer state after reading register. This is a bug!");
                }
                TransferStatus::Failed(DapError::WaitResponse) => {
                    // If ack[1] is set the host must retry the request. So let's do that right away!
                    tracing::debug!(
                        "DAP WAIT (read), retries remaining {}.",
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

                    tracing::debug!("Cleared sticky overrun bit");

                    idle_cycles = std::cmp::min(
                        self.swd_settings().max_retry_idle_cycles_after_wait,
                        idle_cycles * 2,
                    );

                    continue;
                }
                TransferStatus::Failed(DapError::FaultResponse) => {
                    tracing::debug!("DAP FAULT");

                    // A fault happened during operation.

                    // To get a clue about the actual fault we want to read the ctrl register,
                    // which will have the fault status flags set. But we only do this
                    // if we are *not* currently reading the ctrl register, otherwise
                    // this could end up being an endless recursion.

                    if address != Ctrl::ADDRESS {
                        // TODO: This should write SELECT first
                        let response = RawDapAccess::raw_read_register(
                            self,
                            PortType::DebugPort,
                            Ctrl::ADDRESS,
                        )?;
                        let ctrl = Ctrl::try_from(response)?;
                        tracing::warn!(
                            "Reading DAP register {address:#X} failed. Ctrl/Stat register value is: {:#?}",
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
                    } else {
                        tracing::warn!(
                            "Error reading CTRL/STAT register. This should not happen..."
                        );
                    }

                    return Err(DapError::FaultResponse.into());
                }
                // The other errors mean that something went wrong with the protocol itself,
                // so we try to perform a line reset, and recover.
                TransferStatus::Failed(_) => {
                    tracing::debug!("DAP NACK");

                    // Because we clock the SWDCLK line after receving the WAIT response,
                    // the target might be in weird state. If we perform a line reset,
                    // we should be able to recover from this.
                    // line_reset(self, dp)?;

                    // Retry operation again
                    continue;
                }
            }
        }

        // If we land here, the DAP operation timed out.
        tracing::error!("DAP read timeout.");
        Err(ArmError::Timeout)
    }

    fn raw_read_block(
        &mut self,
        port: PortType,
        address: u8,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
        let mut successful_transfers = 0;

        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        let mut transfers = vec![DapTransfer::read(port, address); values.len()];

        'transfer: for _ in 0..self.swd_settings().num_retries_after_wait {
            let transfers = &mut transfers[successful_transfers..];
            if transfers.is_empty() {
                break;
            }

            perform_transfers(self, transfers, idle_cycles)?;

            for result in transfers.iter() {
                match result.status {
                    TransferStatus::Ok => {
                        values[successful_transfers] = result.value;
                        successful_transfers += 1;
                    }
                    TransferStatus::Failed(err) => {
                        tracing::warn!(
                            "Error in access {}/{} of block access: {:?}",
                            successful_transfers + 1,
                            values.len(),
                            anyhow::anyhow!(err)
                        );

                        if err == DapError::WaitResponse {
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

                            tracing::debug!("Retrying access {}", successful_transfers + 1);

                            continue 'transfer;
                        }
                        return Err(err.into());
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

    #[tracing::instrument(level = "debug", skip(self))]
    fn raw_write_register(
        &mut self,
        port: PortType,
        address: u8,
        value: u32,
    ) -> Result<(), ArmError> {
        assert!(address < 0x10, "Invalid register address {:#x}", address);
        let dap_wait_retries = self.swd_settings().num_retries_after_wait;
        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        // Now we try to issue the request until it fails or succeeds.
        // If we timeout we retry a maximum of 5 times.
        for retry in 0..dap_wait_retries {
            let mut transfers = [DapTransfer::write(port, address, value)];

            perform_transfers(self, &mut transfers, idle_cycles)?;

            match transfers[0].status {
                TransferStatus::Ok => return Ok(()),
                TransferStatus::Pending => {
                    panic!("Unexpected transfer state after writing register. This is a bug!");
                }
                TransferStatus::Failed(DapError::WaitResponse) => {
                    // If ack[1] is set the host must retry the request. So let's do that right away!
                    tracing::debug!(
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

                    tracing::debug!("Cleared sticky overrun bit");

                    idle_cycles = std::cmp::min(
                        self.swd_settings().max_retry_idle_cycles_after_wait,
                        idle_cycles * 2,
                    );

                    continue;
                }
                TransferStatus::Failed(DapError::FaultResponse) => {
                    tracing::warn!("DAP FAULT");
                    // A fault happened during operation.

                    // To get a clue about the actual fault we read the ctrl register,
                    // which will have the fault status flags set.

                    let response =
                        RawDapAccess::raw_read_register(self, PortType::DebugPort, Ctrl::ADDRESS)?;

                    let ctrl = Ctrl::try_from(response)?;
                    tracing::warn!(
                        "Writing DAP register {address:#X} failed. Ctrl/Stat register value is: {:#?}",
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
                    tracing::debug!("DAP NACK");

                    // Because we clock the SWDCLK line after receving the WAIT response,
                    // the target might be in weird state. If we perform a line reset,
                    // we should be able to recover from this.
                    line_reset(self)?;

                    // Retry operation
                    continue;
                }
            }
        }

        // If we land here, the DAP operation timed out.
        tracing::error!("DAP write timeout.");
        Err(ArmError::Timeout)
    }

    fn raw_write_block(
        &mut self,
        port: PortType,
        address: u8,
        values: &[u32],
    ) -> Result<(), ArmError> {
        let mut successful_transfers = 0;

        let mut idle_cycles = std::cmp::max(1, self.swd_settings().num_idle_cycles_between_writes);

        let mut transfers = values
            .iter()
            .map(|v| DapTransfer::write(port, address, *v))
            .collect::<Vec<_>>();

        'transfer: for _ in 0..self.swd_settings().num_retries_after_wait {
            let transfers = &mut transfers[successful_transfers..];
            if transfers.is_empty() {
                break;
            }

            perform_transfers(self, transfers, idle_cycles)?;

            for result in transfers.iter() {
                match result.status {
                    TransferStatus::Ok => successful_transfers += 1,
                    TransferStatus::Failed(err) => {
                        tracing::debug!(
                            "Error in access {}/{} of block access: {}",
                            successful_transfers + 1,
                            values.len(),
                            err
                        );

                        if err == DapError::WaitResponse {
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

                            tracing::debug!("Retrying access {}", successful_transfers + 1);

                            continue 'transfer;
                        }

                        return Err(err.into());
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

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        RawProtocolIo::swj_pins(self, pin_out, pin_select, pin_wait)
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn jtag_sequence(&mut self, bit_len: u8, tms: bool, bits: u64) -> Result<(), DebugProbeError> {
        let bits = (0..bit_len).map(|i| (bits >> i) & 1 == 1);

        self.jtag_shift_tdi(tms, bits)?;

        Ok(())
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        let protocol = self.active_protocol().unwrap();

        let io_sequence = IoSequence::from_bytes(&bits.to_le_bytes(), bit_len as usize);
        send_sequence(self, protocol, &io_sequence)
    }

    fn core_status_notification(&mut self, _: crate::CoreStatus) -> Result<(), DebugProbeError> {
        Ok(())
    }
}

fn send_sequence<P: RawProtocolIo + JTAGAccess>(
    probe: &mut P,
    protocol: WireProtocol,
    sequence: &IoSequence,
) -> Result<(), DebugProbeError> {
    match protocol {
        WireProtocol::Jtag => {
            // Swj sequences should be shifted out to tms, since that is the pin
            // shared between swd and jtag modes.
            probe.jtag_shift_tms(sequence.io_bits(), false)?;
        }
        WireProtocol::Swd => {
            probe.swd_io(sequence.direction_bits(), sequence.io_bits())?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod test {

    use std::iter;

    use crate::{
        architecture::arm::{PortType, RawDapAccess},
        error::Error,
        probe::{DebugProbe, DebugProbeError, JTAGAccess, ScanChainElement, WireProtocol},
    };

    use super::{
        parse_jtag_response, ProbeStatistics, RawProtocolIo, SwdSettings, JTAG_ABORT_IR_VALUE,
        JTAG_ACCESS_PORT_IR_VALUE, JTAG_DEBUG_PORT_IR_VALUE, JTAG_DR_BIT_LENGTH, JTAG_STATUS_OK,
        JTAG_STATUS_WAIT,
    };

    use bitvec::prelude::*;

    #[allow(dead_code)]
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
        direction_input: Option<Vec<bool>>,
        io_input: Option<Vec<bool>>,
        transfer_responses: Vec<Vec<bool>>,
        jtag_transactions: Vec<ExpectedJtagTransaction>,

        expected_transfer_count: usize,
        performed_transfer_count: usize,

        swd_settings: SwdSettings,
        probe_statistics: ProbeStatistics,

        protocol: WireProtocol,

        idle_cycles: u8,
    }

    impl MockJaylink {
        fn new() -> Self {
            Self {
                direction_input: None,
                io_input: None,
                transfer_responses: vec![vec![]],
                jtag_transactions: vec![],

                expected_transfer_count: 1,
                performed_transfer_count: 0,

                swd_settings: SwdSettings::default(),
                probe_statistics: ProbeStatistics::default(),

                protocol: WireProtocol::Swd,

                idle_cycles: 0,
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

            let mut response = BitVec::<usize, Lsb0>::repeat(false, write_length);

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

        fn add_jtag_abort(&mut self) {
            let expected = ExpectedJtagTransaction {
                ir_address: JTAG_ABORT_IR_VALUE,
                address: 0,
                value: 0,
                read: false,
                result: 0,
            };

            self.jtag_transactions.push(expected);
            self.expected_transfer_count += 1;
        }

        fn add_jtag_response(
            &mut self,
            port: PortType,
            address: u32,
            read: bool,
            acknowlege: DapAcknowledge,
            output_value: u32,
            input_value: u32,
        ) {
            let mut response = (output_value as u64) << 3;

            let status = match acknowlege {
                DapAcknowledge::Ok => JTAG_STATUS_OK,
                DapAcknowledge::Wait => JTAG_STATUS_WAIT,
                _ => 0b111,
            };

            response |= status as u64;

            let expected = ExpectedJtagTransaction {
                ir_address: if port == PortType::DebugPort {
                    JTAG_DEBUG_PORT_IR_VALUE
                } else {
                    JTAG_ACCESS_PORT_IR_VALUE
                },
                address,
                value: input_value,
                read,
                result: response,
            };

            self.jtag_transactions.push(expected);
            self.expected_transfer_count += 1;
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

            let mut response = BitVec::<usize, Lsb0>::repeat(false, write_length);

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

    impl JTAGAccess for MockJaylink {
        fn scan_chain(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn tap_reset(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn read_register(&mut self, _address: u32, _len: u32) -> Result<Vec<u8>, DebugProbeError> {
            todo!()
        }

        fn set_idle_cycles(&mut self, idle_cycles: u8) {
            self.idle_cycles = idle_cycles;
        }

        fn idle_cycles(&self) -> u8 {
            self.idle_cycles
        }

        fn write_register(
            &mut self,
            address: u32,
            data: &[u8],
            len: u32,
        ) -> Result<Vec<u8>, DebugProbeError> {
            let jtag_value = parse_jtag_response(&data[..5]);

            // Always 35 bit transfers
            assert_eq!(len, JTAG_DR_BIT_LENGTH);

            let jtag_transaction = self.jtag_transactions.remove(0);

            assert_eq!(
                jtag_transaction.ir_address,
                address,
                "Address mismatch with {} remaining transactions",
                self.jtag_transactions.len()
            );

            if jtag_transaction.ir_address != JTAG_ABORT_IR_VALUE {
                let value = (jtag_value >> 3) as u32;
                let rnw = jtag_value & 1 == 1;
                let dap_address = ((jtag_value & 0x6) << 1) as u32;

                assert_eq!(dap_address, jtag_transaction.address);
                assert_eq!(rnw, jtag_transaction.read);
                assert_eq!(value, jtag_transaction.value);
            }

            self.performed_transfer_count += 1;

            let ret = jtag_transaction.result;

            Ok(ret.to_le_bytes()[..5].to_vec())
        }
    }

    impl RawProtocolIo for MockJaylink {
        fn jtag_shift_tms<M>(&mut self, _tms: M, _tdi: bool) -> Result<(), DebugProbeError>
        where
            M: IntoIterator<Item = bool>,
        {
            Ok(())
        }

        fn jtag_shift_tdi<I>(&mut self, _tms: bool, _tdi: I) -> Result<(), DebugProbeError>
        where
            I: IntoIterator<Item = bool>,
        {
            Ok(())
        }

        fn swd_io<D, S>(&mut self, dir: D, swdio: S) -> Result<Vec<bool>, DebugProbeError>
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

        fn swj_pins(
            &mut self,
            _pin_out: u32,
            _pin_select: u32,
            _pin_wait: u32,
        ) -> Result<u32, DebugProbeError> {
            Err(DebugProbeError::CommandNotSupportedByProbe {
                command_name: "swj_pins",
            })
        }

        fn swd_settings(&self) -> &SwdSettings {
            &self.swd_settings
        }

        fn probe_statistics(&mut self) -> &mut ProbeStatistics {
            &mut self.probe_statistics
        }
    }

    /// This is just a blanket impl that will crash if used (only relevant in tests,
    /// so no problem as we do not use it) to fulfill the marker requirement.
    impl DebugProbe for MockJaylink {
        fn get_name(&self) -> &str {
            todo!()
        }

        fn speed_khz(&self) -> u32 {
            todo!()
        }

        fn set_speed(&mut self, _speed_khz: u32) -> Result<u32, DebugProbeError> {
            todo!()
        }

        fn set_scan_chain(
            &mut self,
            _scan_chain: Vec<ScanChainElement>,
        ) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
            todo!()
        }

        fn attach(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn detach(&mut self) -> Result<(), Error> {
            todo!()
        }

        fn target_reset(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
            todo!()
        }

        fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
            self.protocol = protocol;

            Ok(())
        }

        fn active_protocol(&self) -> Option<WireProtocol> {
            Some(self.protocol)
        }

        fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
            todo!()
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
    fn read_register_jtag() {
        let read_value = 12;

        let mut mock = MockJaylink::new();

        let result = mock.select_protocol(WireProtocol::Jtag);
        assert!(result.is_ok());

        // Read request
        mock.add_jtag_response(PortType::AccessPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(
            PortType::DebugPort,
            12,
            true,
            DapAcknowledge::Ok,
            read_value,
            0,
        );
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

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
    fn read_register_with_wait_response_jtag() {
        let read_value = 47;
        let mut mock = MockJaylink::new();

        let result = mock.select_protocol(WireProtocol::Jtag);
        assert!(result.is_ok());

        // Read
        mock.add_jtag_response(PortType::AccessPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Wait, 0, 0);
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

        //  When a wait response is received, the sticky overrun bit has to be cleared
        mock.add_jtag_abort();

        // Retry
        mock.add_jtag_response(PortType::AccessPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(
            PortType::DebugPort,
            12,
            true,
            DapAcknowledge::Ok,
            read_value,
            0,
        );
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

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
    fn write_register_jtag() {
        let mut mock = MockJaylink::new();

        let result = mock.select_protocol(WireProtocol::Jtag);
        assert!(result.is_ok());

        mock.add_jtag_response(
            PortType::AccessPort,
            4,
            false,
            DapAcknowledge::Ok,
            0x0,
            0x123,
        );
        mock.add_jtag_response(
            PortType::DebugPort,
            12,
            true,
            DapAcknowledge::Ok,
            0x123,
            0x0,
        );
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

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

    #[test]
    fn write_register_with_wait_response_jtag() {
        let mut mock = MockJaylink::new();

        let result = mock.select_protocol(WireProtocol::Jtag);
        assert!(result.is_ok());

        mock.add_jtag_response(
            PortType::AccessPort,
            4,
            false,
            DapAcknowledge::Ok,
            0x0,
            0x123,
        );
        mock.add_jtag_response(
            PortType::DebugPort,
            12,
            true,
            DapAcknowledge::Wait,
            0x0,
            0x0,
        );
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

        // Expect a Write to the ABORT register.
        mock.add_jtag_abort();

        // Second try to write register.
        mock.add_jtag_response(
            PortType::AccessPort,
            4,
            false,
            DapAcknowledge::Ok,
            0x0,
            0x123,
        );
        mock.add_jtag_response(
            PortType::DebugPort,
            12,
            true,
            DapAcknowledge::Ok,
            0x123,
            0x0,
        );
        // Check CTRL
        mock.add_jtag_response(PortType::DebugPort, 4, true, DapAcknowledge::Ok, 0, 0);
        mock.add_jtag_response(PortType::DebugPort, 12, true, DapAcknowledge::Ok, 0, 0);

        mock.raw_write_register(PortType::AccessPort, 4, 0x123)
            .expect("Failed to write register");
    }

    /// Test the correct handling of several transfers, with
    /// the appropriate extra reads added as necessary.
    mod transfer_handling {
        use super::{
            super::{perform_transfers, DapTransfer, TransferStatus},
            DapAcknowledge, MockJaylink,
        };
        use crate::architecture::arm::PortType;

        #[test]
        fn single_dp_register_read() {
            let register_value = 32354;

            let mut transfers = vec![DapTransfer::read(PortType::DebugPort, 0)];

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

            let mut transfers = vec![DapTransfer::read(PortType::AccessPort, 0)];

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
                DapTransfer::read(PortType::AccessPort, 4),
                DapTransfer::read(PortType::DebugPort, 3),
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
                DapTransfer::read(PortType::DebugPort, 3),
                DapTransfer::read(PortType::AccessPort, 4),
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
                DapTransfer::read(PortType::AccessPort, 4),
                DapTransfer::read(PortType::AccessPort, 4),
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
                DapTransfer::read(PortType::DebugPort, 4),
                DapTransfer::read(PortType::DebugPort, 4),
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
            let mut transfers = vec![DapTransfer::write(PortType::DebugPort, 0, 0x1234_5678)];

            let mut mock = MockJaylink::new();
            let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );

            // To verify that the write was successful, an additional read is performed.
            mock.add_idle_cycles(mock.swd_settings.idle_cycles_after_transfer);

            perform_transfers(&mut mock, &mut transfers, idle_cycles)
                .expect("Failed to perform transfer");

            let transfer_result = &transfers[0];

            assert_eq!(transfer_result.status, TransferStatus::Ok);
        }

        #[test]
        fn single_ap_register_write() {
            let mut transfers = vec![DapTransfer::write(PortType::AccessPort, 0, 0x1234_5678)];

            let mut mock = MockJaylink::new();

            let idle_cycles = mock.swd_settings.num_idle_cycles_between_writes;

            mock.add_write_response(
                DapAcknowledge::Ok,
                mock.swd_settings.num_idle_cycles_between_writes,
            );

            // To verify that the write was successful, an additional read is performed.
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
                DapTransfer::write(PortType::AccessPort, 0, 0x1234_5678),
                DapTransfer::write(PortType::AccessPort, 0, 0xABABABAB),
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
