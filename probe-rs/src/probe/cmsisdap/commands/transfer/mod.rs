pub mod configure;

use std::iter;

use super::{CommandId, Request, SendError};
use crate::architecture::arm::PortAddress;
use scroll::{Pread, Pwrite, LE};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RW {
    R = 1,
    W = 0,
}

/// Contains information about requested access from host debugger.
#[allow(non_snake_case)]
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
    pub fn new(address: PortAddress, rw: RW, data: Option<u32>) -> Self {
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
    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = (self.APnDP as u8)
            | (self.RnW as u8) << 1
            | u8::from(self.A2) << 2
            | u8::from(self.A3) << 3
            | u8::from(self.value_match) << 4
            | u8::from(self.match_mask) << 5
            | u8::from(self.td_timestamp_request) << 7;
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
#[allow(non_snake_case)]
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
}

impl TransferRequest {
    pub fn empty() -> Self {
        Self {
            dap_index: 0,
            transfers: vec![],
        }
    }

    pub fn read<T: Into<PortAddress>>(address: T) -> Self {
        let mut req = Self::empty();
        req.add_read(address.into());
        req
    }

    pub fn write<T: Into<PortAddress>>(address: T, data: u32) -> Self {
        let mut req = Self::empty();
        req.add_write(address.into(), data);
        req
    }

    pub fn add_read(&mut self, address: PortAddress) {
        self.transfers
            .push(InnerTransferRequest::new(address, RW::R, None));
    }

    pub fn add_write(&mut self, address: PortAddress, data: u32) {
        self.transfers
            .push(InnerTransferRequest::new(address, RW::W, Some(data)));
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
            protocol_error: buffer[1] & 0x8 != 0,
            _value_mismatch: buffer[1] & 0x10 != 0,
        };
        let mut buffer = &buffer[2..];

        let mut transfers = Vec::with_capacity(transfer_count);
        if transfer_count > 0 {
            let acks = iter::repeat(Ack::Ok)
                .take(transfer_count - 1)
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
    /// TODO: ??????????????????????? Docs are weird?
    /// OK (for SWD protocol), OK or FAULT (for JTAG protocol),
    Ok = 1,
    Wait = 2,
    Fault = 4,
    #[allow(clippy::enum_variant_names)]
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

#[derive(Debug)]
pub(crate) struct TransferBlockRequest {
    /// Zero-based device index of the selected JTAG device. For SWD mode the
    /// value is ignored.
    dap_index: u8,
    /// Number of transfers
    transfer_count: u16,

    /// Information about requested access
    transfer_request: InnerTransferBlockRequest,

    /// Register values to write for writes
    transfer_data: Vec<u32>,
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

        size += self.transfer_request.to_bytes(buffer, 3)?;

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
        let transfer_response = buffer
            .pread_with(2, LE)
            .map_err(|_| SendError::NotEnoughData)?;

        let mut data = Vec::with_capacity(transfer_count as usize);

        let num_transfers = (buffer.len() - 3) / 4;

        tracing::debug!(
            "Expected {} responses, got {} responses with data..",
            transfer_count,
            num_transfers
        );

        // if it's a read, process the read data.
        // If it's a write, there's no interesting data in the response.
        if self.transfer_request.r_n_w == RW::R {
            for data_offset in 0..transfer_count as usize {
                data.push(
                    buffer
                        .pread_with(3 + data_offset * 4, LE)
                        .map_err(|_| SendError::NotEnoughData)?,
                );
            }
        }

        Ok(TransferBlockResponse {
            _transfer_count: transfer_count,
            transfer_response,
            transfer_data: data,
        })
    }
}

impl TransferBlockRequest {
    pub(crate) fn write_request(address: PortAddress, data: Vec<u32>) -> Self {
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

    pub(crate) fn read_request(address: PortAddress, read_count: u16) -> Self {
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

#[derive(Debug)]
struct InnerTransferBlockRequest {
    ap_n_dp: bool,
    r_n_w: RW,
    a2: bool,
    a3: bool,
}

impl InnerTransferBlockRequest {
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize, SendError> {
        buffer[offset] = (self.ap_n_dp as u8)
            | (self.r_n_w as u8) << 1
            | u8::from(self.a2) << 2
            | u8::from(self.a3) << 3;
        Ok(1)
    }
}

#[derive(Debug)]
pub(crate) struct TransferBlockResponse {
    _transfer_count: u16,
    pub transfer_response: u8,
    pub transfer_data: Vec<u32>,
}
