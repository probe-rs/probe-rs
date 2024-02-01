use std::io::Write;
use std::net::TcpStream;

#[derive(Debug)]
pub struct TcpPublisher {
    pub address: String,
    pub socket: Option<TcpStream>,
}

impl TcpPublisher {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            socket: None,
        }
    }

    pub fn send(&mut self, bytes: &[u8]) {
        if self.socket.is_none() {
            // Try to connect if there is no socket
            if let Ok(stream) = TcpStream::connect(self.address.clone()) {
                self.socket = Some(stream);
            }
        }

        if let Some(socket) = self.socket.as_mut() {
            if socket.write(bytes).is_err() {
                // Discard socket on error. Try reconnect next time.
                self.socket = None;
            }
        }
    }
}
