use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};

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

/// Listens on a TCP socket and hands whatever a connected client sends to a down channel.
///
/// The counterpart to [`TcpPublisher`]: instead of streaming target output out, this feeds
/// host input in. Everything is non-blocking so it can be polled from the main loop.
#[derive(Debug)]
pub struct TcpSubscriber {
    address: SocketAddr,
    listener: Option<TcpListener>,
    socket: Option<TcpStream>,
}

impl TcpSubscriber {
    pub fn new(address: SocketAddr) -> Self {
        Self {
            address,
            listener: None,
            socket: None,
        }
    }

    /// Returns any bytes received since the last call. Never blocks.
    ///
    /// Binds the listener lazily and accepts a single client at a time; once that client
    /// disconnects, the next one can connect.
    pub fn recv(&mut self) -> Vec<u8> {
        if self.listener.is_none() {
            match TcpListener::bind(self.address) {
                Ok(listener) => {
                    // A failed non-blocking switch would make accept() block the UI, so bail.
                    if listener.set_nonblocking(true).is_err() {
                        return Vec::new();
                    }
                    self.listener = Some(listener);
                }
                Err(_) => return Vec::new(),
            }
        }

        if self.socket.is_none()
            && let Some(listener) = self.listener.as_ref()
            && let Ok((stream, _)) = listener.accept()
            && stream.set_nonblocking(true).is_ok()
        {
            self.socket = Some(stream);
        }

        let mut received = Vec::new();
        if let Some(socket) = self.socket.as_mut() {
            let mut buf = [0u8; 1024];
            loop {
                match socket.read(&mut buf) {
                    // A clean 0-byte read means the peer closed; drop it and wait for the next.
                    Ok(0) => {
                        self.socket = None;
                        break;
                    }
                    Ok(n) => received.extend_from_slice(&buf[..n]),
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => {
                        self.socket = None;
                        break;
                    }
                }
            }
        }
        received
    }
}
