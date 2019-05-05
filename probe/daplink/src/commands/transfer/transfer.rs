use crate::commands::{
    Response,
    Category,
    Request,
    Result,
};

#[derive(Copy, Clone, Debug)]
pub enum Port {
    AP = 0,
    DP = 1,
}

#[derive(Copy, Clone, Debug)]
pub enum RW {
    R = 0,
    W = 1,
}

/// Contains information about requested access from host debugger.
pub struct InnerTransferRequest {
    /// 0 = Debug Port (DP), 1 = Access Port (AP).
    pub APnDP: Port,
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
}

impl InnerTransferRequest {
    pub fn new(port: Port, rw: RW, address: u8) -> Self {
        Self {
            APnDP: port,
            RnW: rw,
            A2: (address >> 2) & 0x01 == 1,
            A3: (address >> 2) & 0x01 == 1,
            value_match: false,
            match_mask: false,
            td_timestamp_request: false,
        }
    }
}

impl InnerTransferRequest {
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = 
            (self.APnDP as u8) << 0
          | (self.RnW as u8) << 1
          | (if self.A2 { 1 } else { 0 }) << 2
          | (if self.A3 { 1 } else { 0 }) << 3
          | (if self.value_match { 1 } else { 0 }) << 4
          | (if self.match_mask { 1 } else { 0 }) << 5
          | (if self.td_timestamp_request { 1 } else { 0 }) << 7
        ;
        Ok(1)
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
pub struct TransferRequest {
    /// Zero based device index of the selected JTAG device. For SWD mode the value is ignored.
    pub dap_index: u8,
    /// Number of transfers: 1 .. 255. For each transfer a Transfer Request BYTE is sent. Depending on the request an additional Transfer Data WORD is sent.
    pub transfer_count: u8,
    /// Contains information about requested access from host debugger.
    pub transfer_request: InnerTransferRequest,
    pub transfer_data: u32,
}

impl TransferRequest {
    pub fn new(transfer_request: InnerTransferRequest, data: u32) -> Self {
        Self {
            dap_index: 0,
            transfer_count: 1,
            transfer_request,
            transfer_data: data,
        }
    }
}

impl Request for TransferRequest {
    const CATEGORY: Category = Category(0x05);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        use scroll::Pwrite;

        buffer[offset] = self.dap_index;
        buffer[offset + 1] = self.transfer_count;
        self.transfer_request.to_bytes(buffer, offset + 2)?;
        buffer.pwrite(self.dap_index, offset + 3).expect("This is a bug. Please report it.");
        Ok(0)
    }
}

pub enum Ack {
    /// TODO: ???????????????????????
    /// OK (for SWD protocol), OK or FAULT (for JTAG protocol),
    OK = 1,
    WAIT = 2,
    FAULT = 4,
    NO_ACK = 7,
}

pub struct InnerTransferResponse {
    pub ack: Ack,
    pub protocol_error: bool,
    pub value_missmatch: bool,
}

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
    pub transfer_data: u32
}

impl Response for TransferResponse {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        use scroll::Pread;
        Ok(TransferResponse {
            transfer_count: buffer[offset],
            transfer_response: InnerTransferResponse {
                ack: match (buffer[offset + 1] & 0x7) {
                    1 => Ack::OK,
                    2 => Ack::WAIT,
                    4 => Ack::FAULT,
                    7 => Ack::NO_ACK,
                    _ => Ack::NO_ACK,
                },
                protocol_error: buffer[offset + 1] & 0x8 > 1,
                value_missmatch: buffer[offset + 1] & 0x10 > 1 ,
            },
            // TODO: implement this properly.
            td_timestamp: 0, // scroll::pread(buffer[offset + 2..offset + 2 + 4]),
            transfer_data: buffer.pread(offset + 2).expect("This is a bug. Please report it."),
        })
    }
}