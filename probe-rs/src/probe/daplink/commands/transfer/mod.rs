pub mod configure;

use super::{Category, Request, Response, Result};
use crate::architecture::arm::PortType as ArmPortType;
use anyhow::anyhow;
use scroll::{Pread, Pwrite, LE};

#[derive(Copy, Clone, Debug)]
pub enum PortType {
    AP = 1,
    DP = 0,
}

impl From<ArmPortType> for PortType {
    fn from(typ: ArmPortType) -> PortType {
        match typ {
            ArmPortType::DebugPort => PortType::DP,
            ArmPortType::AccessPort(_) => PortType::AP,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum RW {
    R = 1,
    W = 0,
}

/// Contains information about requested access from host debugger.
#[allow(non_snake_case)]
#[derive(Copy, Clone, Debug)]
pub struct InnerTransferRequest {
    /// 0 = Debug PortType (DP), 1 = Access PortType (AP).
    pub APnDP: PortType,
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
    pub fn new(port: PortType, rw: RW, address: u8, data: Option<u32>) -> Self {
        Self {
            APnDP: port,
            RnW: rw,
            A2: (address >> 2) & 0x01 == 1,
            A3: (address >> 3) & 0x01 == 1,
            value_match: false,
            match_mask: false,
            td_timestamp_request: false,
            data,
        }
    }
}

#[test]
fn creating_inner_transfer_request() {
    let req = InnerTransferRequest::new(PortType::DP, RW::W, 0x8, None);

    assert_eq!(true, req.A3);
    assert_eq!(false, req.A2);
}

impl InnerTransferRequest {
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = (self.APnDP as u8)
            | (self.RnW as u8) << 1
            | (if self.A2 { 1 } else { 0 }) << 2
            | (if self.A3 { 1 } else { 0 }) << 3
            | (if self.value_match { 1 } else { 0 }) << 4
            | (if self.match_mask { 1 } else { 0 }) << 5
            | (if self.td_timestamp_request { 1 } else { 0 }) << 7;
        if let Some(data) = self.data {
            let data = data.to_le_bytes();
            buffer[offset + 1..offset + 5].copy_from_slice(&data[..]);
            Ok(5)
        } else {
            Ok(1)
        }
    }
}

/// Read/write single and multiple registers.
///
///The DAP_Transfer Command reads or writes data to CoreSight registers. Each CoreSight register is accessed with a single 32-bit read or write. The CoreSight registers are addressed with DPBANKSEL/APBANKSEL and address lines A2, A3 (A0 = 0 and A1 = 0). This command executes several read/write operations on the selected DP/AP registers. The Transfer Data in the Response are in the order of the Transfer Request in the Command but might be shorter in case of communication failures. The data transfer is aborted on a communication error:
///
///- Protocol Error
///- Target FAULT response
///- Target WAIT responses exceed configured value
///- Value Mismatch (Read Register with Value Match)
#[derive(Debug)]
pub struct TransferRequest {
    /// Zero based device index of the selected JTAG device. For SWD mode the value is ignored.
    pub dap_index: u8,
    /// Number of transfers: 1 .. 255. For each transfer a Transfer Request BYTE is sent. Depending on the request an additional Transfer Data WORD is sent.
    pub transfer_count: u8,
    pub transfers: Vec<InnerTransferRequest>,
}

impl TransferRequest {
    pub fn new(transfers: &[InnerTransferRequest]) -> Self {
        Self {
            dap_index: 0,
            transfer_count: transfers.len() as u8,
            transfers: transfers.into(),
        }
    }
}

impl Request for TransferRequest {
    const CATEGORY: Category = Category(0x05);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        let mut size = 0;

        buffer[offset] = self.dap_index;
        size += 1;

        buffer[offset + 1] = self.transfer_count;
        size += 1;

        for transfer in self.transfers.iter() {
            size += transfer.to_bytes(buffer, offset + size)?;
        }

        Ok(size)
    }
}

#[derive(Debug)]
pub enum Ack {
    /// TODO: ??????????????????????? Docs are weird?
    /// OK (for SWD protocol), OK or FAULT (for JTAG protocol),
    Ok = 1,
    Wait = 2,
    Fault = 4,
    NoAck = 7,
}

#[derive(Debug)]
pub struct InnerTransferResponse {
    pub ack: Ack,
    pub protocol_error: bool,
    pub value_missmatch: bool,
}

#[derive(Debug)]
pub struct TransferResponse {
    /// Number of transfers: 1 .. 255 that are executed.
    pub transfer_count: u8,
    /// Contains information about last response from target Device.
    pub transfer_response: InnerTransferResponse,
    /// Current Test Domain Timer value is added before each Transfer Data word when Transfer Request - bit 7: TD_TimeStamp request is set.
    pub td_timestamp: u32,
    /// register value or match value in the order of the Transfer Request.
    ///- for Read Register transfer request: the register value of the CoreSight register.
    ///- no data is sent for other operations.
    pub transfer_data: u32,
}

impl Response for TransferResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        Ok(TransferResponse {
            transfer_count: buffer[offset],
            transfer_response: InnerTransferResponse {
                ack: match buffer[offset + 1] & 0x7 {
                    1 => Ack::Ok,
                    2 => Ack::Wait,
                    4 => Ack::Fault,
                    7 => Ack::NoAck,
                    _ => Ack::NoAck,
                },
                protocol_error: buffer[offset + 1] & 0x8 > 1,
                value_missmatch: buffer[offset + 1] & 0x10 > 1,
            },
            // TODO: implement this properly.
            td_timestamp: 0, // scroll::pread_with(buffer[offset + 2..offset + 2 + 4], LE),
            transfer_data: buffer
                .pread_with(offset + 2, LE)
                .map_err(|_| anyhow!("This is a bug. Please report it."))?,
        })
    }
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
    const CATEGORY: Category = Category(0x06);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        let mut size = 0;
        buffer[offset] = self.dap_index;
        size += 1;

        buffer
            .pwrite_with(self.transfer_count, offset + 1, LE)
            .map_err(|_| anyhow!("This is a bug. Please report it."))?;
        size += 2;

        size += self.transfer_request.to_bytes(buffer, offset + 3)?;

        let mut data_offset = offset + 4;

        for word in &self.transfer_data {
            buffer.pwrite_with(word, data_offset, LE).map_err(|_| {
                anyhow!(
                    "Failed to write word at data_offset {}. This is a bug. Please report it.",
                    data_offset
                )
            })?;
            data_offset += 4;
            size += 4;
        }

        Ok(size)
    }
}

impl TransferBlockRequest {
    pub(crate) fn write_request(address: u8, port: PortType, data: Vec<u32>) -> Self {
        let inner = InnerTransferBlockRequest {
            ap_n_dp: port,
            r_n_w: RW::W,
            a2: (address >> 2) & 0x01 == 1,
            a3: (address >> 3) & 0x01 == 1,
        };

        TransferBlockRequest {
            dap_index: 0,
            transfer_count: data.len() as u16,
            transfer_request: inner,
            transfer_data: data,
        }
    }

    pub(crate) fn read_request(address: u8, port: PortType, read_count: u16) -> Self {
        let inner = InnerTransferBlockRequest {
            ap_n_dp: port,
            r_n_w: RW::R,
            a2: (address >> 2) & 0x01 == 1,
            a3: (address >> 3) & 0x01 == 1,
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
    ap_n_dp: PortType,
    r_n_w: RW,
    a2: bool,
    a3: bool,
}

impl InnerTransferBlockRequest {
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = (self.ap_n_dp as u8)
            | (self.r_n_w as u8) << 1
            | (if self.a2 { 1 } else { 0 }) << 2
            | (if self.a3 { 1 } else { 0 }) << 3;
        Ok(1)
    }
}

#[derive(Debug)]
pub(crate) struct TransferBlockResponse {
    transfer_count: u16,
    pub transfer_response: u8,
    pub transfer_data: Vec<u32>,
}

impl Response for TransferBlockResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        let transfer_count = buffer
            .pread_with(offset, LE)
            .expect("Failed to read transfer count");
        let transfer_response = buffer
            .pread_with(offset + 2, LE)
            .expect("Failed to read transfer response");

        let mut data = Vec::with_capacity(transfer_count as usize);

        for data_offset in 0..(transfer_count as usize) {
            data.push(
                buffer
                    .pread_with(offset + 3 + data_offset * 4, LE)
                    .map_err(|_| anyhow!("Failed to read value.."))?,
            );
        }

        Ok(TransferBlockResponse {
            transfer_count,
            transfer_response,
            transfer_data: data,
        })
    }
}
