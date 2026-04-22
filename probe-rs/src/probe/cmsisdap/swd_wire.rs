//! Per-transaction CMSIS-DAP SWD wire format.
//!
//! A single DAP transfer — one entry within a `DAP_Transfer` command — is encoded as:
//!
//! * **Request:** one flag byte (APnDP, RnW, A2, A3). For writes, four little-endian
//!   data bytes follow.
//! * **Response:** one ACK byte (1=OK, 2=WAIT, 4=FAULT, 7=NoAck). For successful reads,
//!   four little-endian data bytes follow.
//!
//! The request bytes stored in each batched `Transaction` are *already* in the layout
//! that `DAP_Transfer` expects to receive per transfer, so `execute()` concatenates
//! them verbatim rather than decoding and re-encoding. On the response side, the
//! raw USB bytes are sliced into per-transaction chunks here so handles can parse
//! them with [`decode_read_response`] / [`decode_write_response`].

use crate::architecture::arm::RegisterAddress;
use crate::probe::protocols::swd::SwdError;

use super::commands::transfer::Ack;
use super::commands::{CommandId, Request, SendError};

/// A single logical DAP operation: a read or a write at a register address.
pub(super) enum Operation {
    Read(RegisterAddress),
    Write(RegisterAddress, u32),
}

/// Encode an operation into per-transaction request bytes.
///
/// Returns `(request_bytes, expected_response_len)`.
pub(super) fn encode_request(op: &Operation) -> (Vec<u8>, usize) {
    match op {
        Operation::Read(addr) => {
            // APnDP | RnW=1 | A2 | A3
            let transfer_byte = (addr.is_ap() as u8) | 0x02 | addr.a2_and_3();
            // response: ACK byte + 4 data bytes
            (vec![transfer_byte], READ_RESPONSE_LEN)
        }
        Operation::Write(addr, data) => {
            // APnDP | RnW=0 | A2 | A3
            let transfer_byte = (addr.is_ap() as u8) | addr.a2_and_3();
            let mut bytes = Vec::with_capacity(5);
            bytes.push(transfer_byte);
            bytes.extend_from_slice(&data.to_le_bytes());
            // response: ACK byte only
            (bytes, WRITE_RESPONSE_LEN)
        }
    }
}

/// Returns true if the given per-transaction request bytes describe a read transfer.
pub(super) fn is_read(request: &[u8]) -> bool {
    (request[0] & 0x02) != 0
}

/// Build the per-transaction response bytes that handles will later decode.
///
/// `data` is only used when the request is a read that completed with `Ack::Ok`;
/// writes never carry response data.
pub(super) fn encode_response(is_read: bool, ack: Ack, data: Option<u32>) -> Vec<u8> {
    let ack_byte = ack_to_byte(ack);
    if is_read {
        let mut bytes = Vec::with_capacity(READ_RESPONSE_LEN);
        bytes.push(ack_byte);
        bytes.extend_from_slice(&data.unwrap_or(0).to_le_bytes());
        bytes
    } else {
        vec![ack_byte]
    }
}

/// Decode per-transaction response bytes for a read into a [`Result`].
pub(super) fn decode_read_response(bytes: &[u8]) -> Result<u32, SwdError> {
    match bytes {
        [1, data @ ..] => Ok(u32::from_le_bytes((*data).try_into().unwrap())),
        [2, ..] => Err(SwdError::Wait),
        [4, ..] => Err(SwdError::Fault),
        [7, ..] => Err(SwdError::NoAck),
        _ => unreachable!("invalid CMSIS-DAP response"),
    }
}

/// Decode per-transaction response bytes for a write into a [`Result`].
pub(super) fn decode_write_response(bytes: &[u8]) -> Result<(), SwdError> {
    match bytes {
        [1] => Ok(()),
        [2] => Err(SwdError::Wait),
        [4] => Err(SwdError::Fault),
        [7] => Err(SwdError::NoAck),
        _ => unreachable!("invalid CMSIS-DAP response"),
    }
}

fn ack_to_byte(ack: Ack) -> u8 {
    match ack {
        Ack::Ok => 1,
        Ack::Wait => 2,
        Ack::Fault => 4,
        Ack::NoAck => 7,
    }
}

pub(super) const READ_RESPONSE_LEN: usize = 5;
pub(super) const WRITE_RESPONSE_LEN: usize = 1;

/// A prebuilt `DAP_Transfer` command. Its `body` is the already-assembled payload:
///
/// ```text
/// [dap_index, transfer_count, per_transfer_bytes...]
/// ```
///
/// where the per-transfer bytes are the concatenation of each transaction's stored
/// request bytes, which are already in the exact shape this command expects.
///
/// The response type is the raw reply bytes (minus the command-ID byte stripped
/// by [`send_command`](super::commands::send_command)); the caller splits the
/// response into per-transaction chunks using [`encode_response`].
pub(super) struct RawDapTransfer<'a> {
    pub body: &'a [u8],
}

impl Request for RawDapTransfer<'_> {
    const COMMAND_ID: CommandId = CommandId::Transfer;

    type Response = Vec<u8>;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        if self.body.len() > buffer.len() {
            return Err(SendError::NotEnoughData);
        }
        buffer[..self.body.len()].copy_from_slice(self.body);
        Ok(self.body.len())
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer.len() < 2 {
            return Err(SendError::NotEnoughData);
        }
        Ok(buffer.to_vec())
    }
}
