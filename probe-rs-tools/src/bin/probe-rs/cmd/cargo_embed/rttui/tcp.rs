use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};

#[derive(Debug)]
pub struct TcpPublisher {
    pub address: SocketAddr,
    pub socket: Option<TcpStream>,
}

impl TcpPublisher {
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            socket: None,
        }
    }

    fn ensure_connected(&mut self) -> bool {
        if self.socket.is_none() {
            // Try to connect if there is no socket
            if let Ok(stream) = TcpStream::connect(self.address) {
                // Set non-blocking mode for reading
                if let Err(e) = stream.set_nonblocking(true) {
                    tracing::warn!("Failed to set TCP stream to non-blocking: {e}");
                    return false;
                }
                self.socket = Some(stream);
                true
            } else {
                false
            }
        } else {
            true
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        if !self.ensure_connected() {
            return;
        }

        if let Some(socket) = self.socket.as_mut()
            && socket.write_all(bytes).is_err()
        {
            // Discard socket on error. Try reconnect next time.
            self.socket = None;
        }
    }

    /// Read available data from the TCP stream without blocking.
    /// Returns the number of bytes read, or None if no data is available.
    pub fn try_read(&mut self, buf: &mut [u8]) -> Option<usize> {
        if !self.ensure_connected() {
            return None;
        }

        if let Some(socket) = self.socket.as_mut() {
            match socket.read(buf) {
                Ok(0) => {
                    // Connection closed
                    self.socket = None;
                    None
                }
                Ok(n) => Some(n),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No data available, which is fine
                    None
                }
                Err(_) => {
                    // Other error, discard socket
                    self.socket = None;
                    None
                }
            }
        } else {
            None
        }
    }
}
