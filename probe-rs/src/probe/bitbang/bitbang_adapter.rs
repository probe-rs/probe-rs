use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Abstracts the bit bang encoding scheme to generic read/write/quit functions.  The openocd JTAG
/// documention outlines this protocol: [bitbang](https://github.com/openocd-org/openocd/blob/b6b4f9d46a48aadc1de6bb5152ff4913661c9059/doc/manual/jtag/drivers/remote_bitbang.txt)
#[derive(Debug)]
pub struct BitBangAdapter {
    socket: TcpStream,
}

impl BitBangAdapter {
    pub fn new(mut socket: TcpStream) -> io::Result<Self> {
        // Dump anything that was already in the socket
        let mut junk = vec![];
        socket.set_read_timeout(Some(Duration::from_millis(500)))?;
        socket.set_write_timeout(Some(Duration::from_millis(500)))?;
        let _ = socket.read_to_end(&mut junk);

        Ok(Self { socket })
    }

    // TODO: probably want some way of queuing up a longer message, right now each request
    // (read/write/reset/etc) takes sends 1 tcp packet.
    /// Send a packet over the socket and check that is was written successfully.
    fn send_checked_packet(&mut self, packet: &str) -> io::Result<()> {
        let _written = self.socket.write(packet.as_bytes())?;
        // TODO: verify all bytes were written
        Ok(())
    }

    /// Control the JTAG reset lines
    pub fn reset(&mut self, trst: bool, srst: bool) -> io::Result<()> {
        let packet = match (trst, srst) {
            (false, false) => "r",
            (false, true) => "s",
            (true, false) => "t",
            (true, true) => "u",
        };
        self.send_checked_packet(packet)?;
        Ok(())
    }

    /// Set the value of TCK, TMS, and TDI
    pub fn write(&mut self, tck: bool, tms: bool, tdi: bool) -> io::Result<()> {
        let packet = match (tck, tms, tdi) {
            (false, false, false) => "0",
            (false, false, true) => "1",
            (false, true, false) => "2",
            (false, true, true) => "3",
            (true, false, false) => "4",
            (true, false, true) => "5",
            (true, true, false) => "6",
            (true, true, true) => "7",
        };
        self.send_checked_packet(packet)?;
        Ok(())
    }

    /// read TDO
    pub fn read(&mut self) -> io::Result<bool> {
        self.send_checked_packet("R")?;
        let mut tdo = [1; 1];
        let n_read = self.socket.read(&mut tdo)?;
        if n_read != 1 {
            // TODO proper error
            panic!("Invalid read...")
        }
        // TODO verify n_read is not zero
        Ok(tdo != "0".as_bytes())
    }

    /// Tell the bit bang server we are done sending messages
    pub fn quit(&mut self) -> io::Result<()> {
        self.send_checked_packet("Q")?;
        Ok(())
    }
}
