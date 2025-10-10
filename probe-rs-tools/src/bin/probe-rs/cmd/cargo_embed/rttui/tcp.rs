use std::io::Write;
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

    pub fn send(&mut self, bytes: &[u8]) {
        if self.socket.is_none() {
            // Try to connect if there is no socket
            if let Ok(stream) = TcpStream::connect(self.address) {
                self.socket = Some(stream);
            }
        }

        if let Some(socket) = self.socket.as_mut()
            && socket.write_all(bytes).is_err()
        {
            // Discard socket on error. Try reconnect next time.
            self.socket = None;
        }
    }
}
