pub mod configure;

use std::iter;

use super::{CommandId, Request, SendError};
use crate::architecture::arm::RegisterAddress;
use scroll::{LE, Pread, Pwrite};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RW {
    R = 1,
    W = 0,
}

/// Contains information about requested access from host debugger.
#[expect(non_snake_case)]
#[derive(Clone, Debug)]
struct InnerTransferRequest {
    /// 0 = Debug PortType (DP), 1 = Access PortType (AP).
    pub APnDP: bool,
    /// 0 = Write Register, 1 = Read Register.
    pub RnW: RW,
    /// A2 Register Address bit 2.
    pub A2: bool,
    /// A3 Register Address bit 3.
    pub A3: bool,
    /// (only valid for Read Register): 0 = Normal Read Register, 1 = Read Register with Value Match.
    pub value_match: bool,
    /// (only valid for Write Register): 0 = Normal Write Register, 1 = Write Match Mask (instead of Register).
    pub match_mask: bool,
    /// 0 = No time stamp, 1 = Include time stamp value from Test Domain Timer before every Transfer Data word (restrictions see note).
    pub td_timestamp_request: bool,

    /// Contains the optional data word, only present
    /// for register writes, match mask writes, or value match reads.
    pub data: Option<u32>,
}

impl InnerTransferRequest {
    pub fn new(address: RegisterAddress, rw: RW, data: Option<u32>) -> Self {
        let a2and3 = address.a2_and_3();
        //tracing::warn!("InnerTransferRequest: address_byte: {:x}", address_byte);
        Self {
            APnDP: address.is_ap(),
            RnW: rw,
            A2: (a2and3 >> 2) & 0x01 == 1,
            A3: (a2and3 >> 3) & 0x01 == 1,
            value_match: false,
            match_mask: false,
            td_timestamp_request: false,
            data,
        }
    }
}

#[test]
fn creating_inner_transfer_request() {
    use crate::architecture::arm::dp::{DpRegister, SelectV1};
    let req = InnerTransferRequest::new(SelectV1::ADDRESS.into(), RW::W, None);
    assert!(req.A3);
    assert!(!req.A2);
}

impl InnerTransferRequest {
    /// Bytes this transfer adds to the command, and to its reply.
    fn lengths(&self) -> (usize, usize) {
        let request = 1 + if self.data.is_some() { 4 } else { 0 };
        let response =
            4 * usize::from(self.td_timestamp_request) + 4 * usize::from(self.RnW == RW::R);
        (request, response)
    }

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = (self.APnDP as u8)
            | ((self.RnW as u8) << 1)
            | (u8::from(self.A2) << 2)
            | (u8::from(self.A3) << 3)
            | (u8::from(self.value_match) << 4)
            | (u8::from(self.match_mask) << 5)
            | (u8::from(self.td_timestamp_request) << 7);
        if let Some(data) = self.data {
            let data = data.to_le_bytes();
            buffer[1..5].copy_from_slice(&data[..]);
            Ok(5)
        } else {
            Ok(1)
        }
    }
}
/// Response to an InnerTransferRequest.
#[derive(Clone, Debug)]
pub struct InnerTransferResponse {
    /// Test Domain Timestamp. Will be `Some` if `td_timestamp_request` was set on the request.
    pub td_timestamp: Option<u32>,
    /// Response data. Will be `Some` if the request was a read.
    pub data: Option<u32>,
}

impl InnerTransferResponse {
    fn from_bytes(
        req: &InnerTransferRequest,
        ack: Ack,
        buffer: &[u8],
    ) -> Result<(Self, usize), SendError> {
        let mut resp = Self {
            td_timestamp: None,
            data: None,
        };

        let mut offset = 0;
        // Only expect response data if the transfer was successful
        if ack == Ack::Ok {
            if req.td_timestamp_request {
                if buffer.len() < offset + 4 {
                    return Err(SendError::NotEnoughData);
                }
                resp.td_timestamp = Some(buffer.pread_with(offset, LE).unwrap());
                offset += 4;
            }
            if req.RnW == RW::R {
                if buffer.len() < offset + 4 {
                    return Err(SendError::NotEnoughData);
                }
                resp.data = Some(buffer.pread_with(offset, LE).unwrap());
                offset += 4;
            }
        }

        Ok((resp, offset))
    }
}

/// Bytes of a `DAP_Transfer` command, and of its reply, that are not transfers.
///
/// The command carries its id, the DAP index and the transfer count; the reply answers with the
/// command id, the transfer count and the last transfer's acknowledgement.
const HEADER_LEN: usize = 3;

/// Read/write single and multiple registers.
///
/// The DAP_Transfer Command reads or writes data to CoreSight registers.
/// Each CoreSight register is accessed with a single 32-bit read or write.
/// The CoreSight registers are addressed with DPBANKSEL/APBANKSEL and address lines A2, A3 (A0 = 0 and A1 = 0).
/// This command executes several read/write operations on the selected DP/AP registers.
/// The Transfer Data in the Response are in the order of the Transfer Request in the Command but might be shorter in case of communication failures.
/// The data transfer is aborted on a communication error:
///
/// - Protocol Error
/// - Target FAULT response
/// - Target WAIT responses exceed configured value
/// - Value Mismatch (Read Register with Value Match)
#[derive(Debug)]
pub struct TransferRequest {
    /// Zero based device index of the selected JTAG device. For SWD mode the value is ignored.
    pub dap_index: u8,
    transfers: Vec<InnerTransferRequest>,
    request_len: usize,
    response_len: usize,
}

impl TransferRequest {
    pub fn empty() -> Self {
        Self {
            dap_index: 0,
            transfers: vec![],
            request_len: HEADER_LEN,
            response_len: HEADER_LEN,
        }
    }

    /// Bytes this command and its reply occupy in a packet.
    pub fn packet_lengths(&self) -> (usize, usize) {
        (self.request_len, self.response_len)
    }

    /// Transfers in this command.
    pub fn len(&self) -> usize {
        self.transfers.len()
    }

    /// True when a `packet_size` packet still has room for one more `rw` transfer.
    ///
    /// A read and a write cost different things, and in different directions: both cost a request
    /// byte, a write adds four more to the command, and a read adds four to the reply.
    ///
    /// The transfer count travels in a single byte, so a command is capped at [`u8::MAX`]
    /// transfers however large the packet is.
    pub fn has_room_for(&self, rw: RW, packet_size: u16) -> bool {
        if self.len() >= u8::MAX as usize {
            return false;
        }

        // Price the transfer with the encoder rather than restating what it writes. Only the
        // direction changes the size, so the address here is arbitrary.
        let (added_request, added_response) = InnerTransferRequest::new(
            RegisterAddress::ApRegister(0),
            rw,
            (rw == RW::W).then_some(0),
        )
        .lengths();

        let capacity = packet_size as usize;
        let (request, response) = self.packet_lengths();
        request + added_request <= capacity && response + added_response <= capacity
    }

    pub fn read<T: Into<RegisterAddress>>(address: T) -> Self {
        let mut req = Self::empty();
        req.add_read(address.into());
        req
    }

    pub fn write<T: Into<RegisterAddress>>(address: T, data: u32) -> Self {
        let mut req = Self::empty();
        req.add_write(address.into(), data);
        req
    }

    pub fn add_read(&mut self, address: RegisterAddress) {
        self.push(InnerTransferRequest::new(address, RW::R, None));
    }

    pub fn add_write(&mut self, address: RegisterAddress, data: u32) {
        self.push(InnerTransferRequest::new(address, RW::W, Some(data)));
    }

    fn push(&mut self, transfer: InnerTransferRequest) {
        let (request, response) = transfer.lengths();
        self.request_len += request;
        self.response_len += response;
        self.transfers.push(transfer);
    }
}

impl Request for TransferRequest {
    const COMMAND_ID: CommandId = CommandId::Transfer;

    type Response = TransferResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        let mut size = 0;

        buffer[0] = self.dap_index;
        size += 1;

        buffer[1] = self.transfers.len() as u8;
        size += 1;

        for transfer in self.transfers.iter() {
            size += transfer.to_bytes(&mut buffer[size..])?;
        }

        Ok(size)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer.len() < 2 {
            return Err(SendError::NotEnoughData);
        }
        let transfer_count = buffer[0] as usize;
        if transfer_count > self.transfers.len() {
            tracing::error!("Transfer count larger than requested number of transfers");
            return Err(SendError::UnexpectedAnswer);
        }

        let last_transfer_response = LastTransferResponse {
            ack: match buffer[1] & 0x7 {
                1 => Ack::Ok,
                2 => Ack::Wait,
                4 => Ack::Fault,
                7 => Ack::NoAck,
                _ => Ack::NoAck,
            },
            protocol_error: buffer[1] & (1 << 3) != 0,
            _value_mismatch: buffer[1] & (1 << 4) != 0,
        };
        let mut buffer = &buffer[2..];

        let mut transfers = Vec::with_capacity(transfer_count);
        if transfer_count > 0 {
            let acks = std::iter::repeat_n(Ack::Ok, transfer_count - 1)
                .chain(iter::once(last_transfer_response.ack))
                .zip(self.transfers.iter());

            for (ack, req) in acks {
                let (resp, len) = InnerTransferResponse::from_bytes(req, ack, buffer)?;
                transfers.push(resp);
                buffer = &buffer[len..];
            }
        }

        Ok(TransferResponse {
            last_transfer_response,
            transfers,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Ack {
    // TODO: ??????????????????????? Docs are weird?
    /// OK (for SWD protocol), OK or FAULT (for JTAG protocol),
    Ok = 1,
    Wait = 2,
    Fault = 4,
    #[expect(clippy::enum_variant_names)]
    NoAck = 7,
}

#[derive(Debug)]
pub struct LastTransferResponse {
    pub ack: Ack,
    pub protocol_error: bool,
    pub _value_mismatch: bool,
}

#[derive(Debug)]
pub struct TransferResponse {
    /// Contains information about last response from target Device.
    pub last_transfer_response: LastTransferResponse,
    /// Responses to each requested transfer in `TransferRequest`. May be shorter than
    /// `TransferRequest::transfers` in case of communication failure.
    pub transfers: Vec<InnerTransferResponse>,
}

/// Repeats one access, so that one packet carries more words than
/// [`TransferRequest`] can.
#[derive(Debug)]
pub struct TransferBlockRequest {
    /// Zero-based device index of the selected JTAG device. For SWD mode the
    /// value is ignored.
    pub dap_index: u8,

    /// Number of transfers
    pub transfer_count: u16,

    /// Information about requested access
    pub transfer_request: InnerTransferBlockRequest,

    /// Register values to write for writes
    pub transfer_data: Vec<u32>,
}

impl Request for TransferBlockRequest {
    const COMMAND_ID: CommandId = CommandId::TransferBlock;

    type Response = TransferBlockResponse;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        let mut size = 0;
        buffer[0] = self.dap_index;
        size += 1;

        buffer
            .pwrite_with(self.transfer_count, 1, LE)
            .expect("Buffer for CMSIS-DAP command is too small. This is a bug, please report it.");
        size += 2;

        size += self.transfer_request.as_bytes(buffer, 3)?;

        let mut data_offset = 4;

        for word in &self.transfer_data {
            buffer.pwrite_with(word, data_offset, LE).expect(
                "Buffer for CMSIS-DAP command is too small. This is a bug, please report it.",
            );
            data_offset += 4;
            size += 4;
        }

        Ok(size)
    }

    fn parse_response(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        let transfer_count = buffer
            .pread_with(0, LE)
            .map_err(|_| SendError::NotEnoughData)?;
        let raw_transfer_response: u8 = buffer
            .pread_with(2, LE)
            .map_err(|_| SendError::NotEnoughData)?;

        let mut data = Vec::with_capacity(transfer_count as usize);

        // A read holds one word for every transfer that ran. A write holds no
        // data.
        if self.transfer_request.r_n_w == RW::R {
            for data_offset in 0..transfer_count as usize {
                data.push(
                    buffer
                        .pread_with(3 + data_offset * 4, LE)
                        .map_err(|_| SendError::NotEnoughData)?,
                );
            }
        }

        let ack = match raw_transfer_response & 0b111 {
            1 => Ack::Ok,
            2 => Ack::Wait,
            4 => Ack::Fault,
            7 => Ack::NoAck,
            ack => {
                tracing::warn!("Unexpected response to SWD/JTAG transfer: {ack:x}");
                Ack::NoAck
            }
        };

        let protocol_error = (raw_transfer_response & (1 << 3)) != 0;

        let transfer_response = LastTransferResponse {
            ack,
            protocol_error,
            // Not applicable for block transfer
            _value_mismatch: false,
        };

        Ok(TransferBlockResponse {
            transfer_count,
            transfer_response,
            transfer_data: data,
        })
    }
}

impl TransferBlockRequest {
    pub fn write_request(address: RegisterAddress, data: Vec<u32>) -> Self {
        let inner = InnerTransferBlockRequest {
            ap_n_dp: address.is_ap(),
            r_n_w: RW::W,
            a2: address.a2(),
            a3: address.a3(),
        };

        TransferBlockRequest {
            dap_index: 0,
            transfer_count: data.len() as u16,
            transfer_request: inner,
            transfer_data: data,
        }
    }

    pub fn read_request(address: RegisterAddress, read_count: u16) -> Self {
        let inner = InnerTransferBlockRequest {
            ap_n_dp: address.is_ap(),
            r_n_w: RW::R,
            a2: address.a2(),
            a3: address.a3(),
        };

        TransferBlockRequest {
            dap_index: 0,
            transfer_count: read_count,
            transfer_request: inner,
            transfer_data: Vec::new(),
        }
    }
}

/// The access that a [`TransferBlockRequest`] repeats.
#[derive(Debug, Copy, Clone)]
pub struct InnerTransferBlockRequest {
    ap_n_dp: bool,
    r_n_w: RW,
    a2: bool,
    a3: bool,
}

impl InnerTransferBlockRequest {
    fn as_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize, SendError> {
        buffer[offset] = (self.ap_n_dp as u8)
            | ((self.r_n_w as u8) << 1)
            | (u8::from(self.a2) << 2)
            | (u8::from(self.a3) << 3);
        Ok(1)
    }
}

/// The response to a [`TransferBlockRequest`].
#[derive(Debug)]
pub struct TransferBlockResponse {
    pub transfer_count: u16,
    pub transfer_response: LastTransferResponse,
    pub transfer_data: Vec<u32>,
}

#[cfg(test)]
mod packet_length_tests {
    use super::*;

    fn read_and_write() -> TransferRequest {
        let mut request = TransferRequest::empty();
        request.add_read(RegisterAddress::ApRegister(0x0C));
        request.add_write(RegisterAddress::ApRegister(0x04), 0x1234_5678);
        request
    }

    #[test]
    fn the_command_length_is_what_the_encoder_writes() {
        let request = read_and_write();

        let mut buffer = [0u8; 64];
        let written = request.to_bytes(&mut buffer).unwrap();

        // `to_bytes` writes everything in the packet but the command id.
        assert_eq!(request.packet_lengths().0, written + 1);
    }

    #[test]
    fn the_reply_length_is_what_the_parser_consumes() {
        let request = read_and_write();
        let reply_len = request.packet_lengths().1 - 1;

        // Both transfers ran, and the last was acknowledged. Only the read carries data back.
        let mut reply = vec![0u8; reply_len];
        reply[0] = 2;
        reply[1] = Ack::Ok as u8;
        assert!(request.parse_response(&reply).is_ok());

        assert!(matches!(
            request.parse_response(&reply[..reply_len - 1]),
            Err(SendError::NotEnoughData)
        ));
    }

    #[test]
    fn a_packet_takes_more_reads_than_writes() {
        let mut reads = TransferRequest::empty();
        while reads.has_room_for(RW::R, 64) {
            reads.add_read(RegisterAddress::ApRegister(0x0C));
        }

        let mut writes = TransferRequest::empty();
        while writes.has_room_for(RW::W, 64) {
            writes.add_write(RegisterAddress::ApRegister(0x0C), 0);
        }

        assert_eq!(reads.len(), 15);
        assert_eq!(writes.len(), 12);
        assert!(reads.packet_lengths().1 <= 64);
        assert!(writes.packet_lengths().0 <= 64);
    }

    #[test]
    fn no_packet_takes_more_transfers_than_the_count_field() {
        let mut request = TransferRequest::empty();
        while request.has_room_for(RW::R, u16::MAX) {
            request.add_read(RegisterAddress::ApRegister(0x0C));
        }

        assert_eq!(request.len(), u8::MAX as usize);
    }
}
