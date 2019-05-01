pub mod general;

use core::ops::Deref;

type Result<T> = std::result::Result<T, Error>;

enum Status {
    DAPOk = 0x00,
    DAPError = 0xFF,
}

impl Status {
    pub fn from_byte(value: u8) -> Result<Self> {
        match value {
            0x00 => Ok(Status::DAPOk),
            0xFF => Ok(Status::DAPError),
            _ => Err(Error::UnexpectedAnswer),
        }
    }
}

struct Category(u8);

impl Deref for Category {
    type Target = u8;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

trait Request {
    const CATEGORY: Category;

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize>;
}

trait Response: Sized {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self>;
}

enum Error {
    NotEnoughSpace,
    USB,
    UnexpectedAnswer,
    DAPError,
}

fn send_command<Req: Request, Res: Response>(request: Req) -> Result<Res> {
    let buffer = &mut [0u8; 1024];

    // Write the command & request to the buffer.
    // TODO: Error handling & real USB writing.
    buffer[0] = *Req::CATEGORY;
    let size = request.to_bytes(buffer, 1)?;
    let size = request.to_bytes(buffer, size + 2)?;

    // Read back resonse.
    // TODO: Error handling & real USB reading.

    // TODO: fix deref trait to avoid ugly .0
    if buffer[0] == *Req::CATEGORY {
        Res::from_bytes(buffer, 1)
    } else {
        Err(Error::UnexpectedAnswer)
    }
}