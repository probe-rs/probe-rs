use probe_rs::probe::sifliuart::console::SifliUartConsole;
use std::io;

pub struct SifliUartClient {
    console: SifliUartConsole,
    read_buf: Vec<u8>,
}

impl SifliUartClient {
    pub fn new(console: SifliUartConsole) -> Self {
        Self {
            console,
            read_buf: vec![0u8; 4096],
        }
    }

    pub fn poll(&mut self) -> io::Result<&[u8]> {
        let count = self.console.read(&mut self.read_buf)?;
        Ok(&self.read_buf[..count])
    }

    pub fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.console.write(data)
    }

    pub fn channel_name(&self) -> &str {
        "uart"
    }
}
