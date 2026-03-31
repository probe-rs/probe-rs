use super::transport::SifliUartTransport;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
/// Cloneable handle for the SiFli UART console side channel.
pub struct SifliUartConsole {
    transport: Arc<Mutex<SifliUartTransport>>,
}

impl SifliUartConsole {
    pub(super) fn new(transport: Arc<Mutex<SifliUartTransport>>) -> Self {
        Self { transport }
    }

    /// Reads any available console output without framing.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        let mut transport = self.transport.lock().unwrap();
        transport.console_read(buf)
    }

    /// Writes raw console input bytes to the target.
    pub fn write(&self, data: &[u8]) -> io::Result<usize> {
        let mut transport = self.transport.lock().unwrap();
        transport.console_write(data)
    }
}
