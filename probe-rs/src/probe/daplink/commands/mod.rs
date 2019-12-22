pub mod general;
pub mod swd;
pub mod swj;
pub mod transfer;

use core::ops::Deref;

use crate::error::*;

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
            _ => res!(UnexpectedDapAnswer),
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

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize>;
}

pub(crate) trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self>;
}

pub(crate) fn send_command<Req: Request, Res: Response>(
    device: &hidapi::HidDevice,
    request: Req,
) -> Result<Res> {
    // Write the command & request to the buffer.
    // TODO: Error handling & real USB writing.
    let buffer = &mut [0; 24];
    buffer[1] = *Req::CATEGORY;
    let _size = request.to_bytes(buffer, 1 + 1)?;
    device.write(buffer)?;
    log::trace!("Send buffer: {:02X?}", &buffer[..]);

    // Read back resonse.
    // TODO: Error handling & real USB reading.
    let buffer = &mut [0; 24];
    device.read(buffer)?;
    log::trace!("Receive buffer: {:02X?}", &buffer[..]);
    if buffer[0] == *Req::CATEGORY {
        Res::from_bytes(buffer, 1)
    } else {
        res!(UnexpectedDapAnswer)
    }
}
