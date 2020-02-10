mod decoder;
mod publisher;

pub use decoder::TracePacket;
pub use publisher::{ItmPublisher, UpdaterChannel};

use crate::error::Error;

pub trait SwvReader {
    fn read(&mut self) -> Result<Vec<u8>, Error>;
}