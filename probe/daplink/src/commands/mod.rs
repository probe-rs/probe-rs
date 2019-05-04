pub mod general;

use core::ops::Deref;

type Result<T> = std::result::Result<T, Error>;

pub(crate) enum Status {
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

#[derive(Clone, Debug)]
pub(crate) enum Error {
    NotEnoughSpace,
    USB,
    UnexpectedAnswer,
    DAPError,
}

pub(crate) fn send_command<Req: Request, Res: Response>(device_info: &hidapi::HidDeviceInfo, request: Req) -> Result<Res> {
    match hidapi::HidApi::new() {
        Ok(api) => {
            let device = device_info.open_device(&api).unwrap();

            // Write the command & request to the buffer.
            // TODO: Error handling & real USB writing.
            let buffer = &mut [0; 24];
            buffer[0 + 1] = *Req::CATEGORY;
            let size = request.to_bytes(buffer, 1 + 1)?;
            device.write(buffer);
            println!("{:?}", &buffer[..]);

            // Read back resonse.
            // TODO: Error handling & real USB reading.
            let buffer = &mut [0; 24];
            device.read(buffer);
            println!("{:?}", &buffer[..]);
            if buffer[0] == *Req::CATEGORY {
                Res::from_bytes(buffer, 1)
            } else {
                Err(Error::UnexpectedAnswer)
            }
        },
        Err(e) => {
            Err(Error::USB)
        },
    }
}