pub mod general;
pub mod swd;
pub mod swj;
pub mod transfer;

use crate::architecture::arm::DapError;
use crate::DebugProbeError;
use core::ops::Deref;

use thiserror::Error;

pub(crate) type Result<T> = std::result::Result<T, CmsisDapError>;

#[derive(Debug, Error)]
pub enum CmsisDapError {
    #[error("Unexpected answer to command")]
    UnexpectedAnswer,
    #[error("CMSIS-DAP responded with an error")]
    ErrorResponse,
    #[error("Too much data provided for SWJ Sequence command")]
    TooMuchData,
    #[error("Error in the USB HID access: {0}")]
    HidApi(#[from] hidapi::HidError),
    #[error("An error with the DAP communication occured: {0}")]
    Dap(#[from] DapError),
}

impl From<CmsisDapError> for DebugProbeError {
    fn from(error: CmsisDapError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(error))
    }
}

#[derive(Debug)]
pub(crate) enum Status {
    DAPOk = 0x00,
    DAPError = 0xFF,
}

impl Status {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(Status::DAPOk),
            0xFF => Ok(Status::DAPError),
            _ => Err(CmsisDapError::UnexpectedAnswer),
        }
    }
}

pub(crate) struct Category(u8);

impl Deref for Category {
    type Target = u8;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub(crate) trait Request {
    const CATEGORY: Category;

    /// Convert the request to bytes, which can be sent to the Daplink. Returns
    /// the amount of bytes written to the buffer.
    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize>;
}

pub(crate) trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self>;
}

pub(crate) fn send_command<Req: Request, Res: Response>(
    device: &mut std::sync::Mutex<hidapi::HidDevice>,
    request: Req,
) -> Result<Res> {
    const BUFFER_LEN: usize = 100;
    // Write the command & request to the buffer.
    // TODO: Error handling & real USB writing.
    // TODO: Use proper buffer size based on the HID
    //       report count.
    let mut write_buffer = [0; BUFFER_LEN];
    write_buffer[1] = *Req::CATEGORY;
    let mut size = request.to_bytes(&mut write_buffer, 1 + 1)?;
    size += 2;

    // ensure size of packet is at least 64
    // this should be read from the USB HID Record
    size = std::cmp::max(size, 64);

    device.get_mut().unwrap().write(&write_buffer[..size])?;
    log::trace!("Send buffer: {:02X?}", &write_buffer[..size]);

    // Read back resonse.
    // TODO: Error handling & real USB reading.
    let mut read_buffer = [0; BUFFER_LEN];
    device.get_mut().unwrap().read(&mut read_buffer)?;
    log::trace!("Receive buffer: {:02X?}", &read_buffer[..]);
    if read_buffer[0] == *Req::CATEGORY {
        Res::from_bytes(&read_buffer, 1)
    } else {
        Err(CmsisDapError::UnexpectedAnswer)
    }
}
