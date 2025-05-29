use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;

use crate::probe::{
    DebugProbeError, DebugProbeSelector, ProbeCreationError,
    glasgow::mux::{DiscoveryError, hexdump},
};

trait ReadWrite: Read + Write {}

impl ReadWrite for TcpStream {}
#[cfg(unix)]
impl ReadWrite for UnixStream {}

pub struct GlasgowNetDevice(Box<dyn ReadWrite + Send>);

impl GlasgowNetDevice {
    pub fn new_from_selector(selector: &DebugProbeSelector) -> Result<Self, ProbeCreationError> {
        let stream: Box<dyn ReadWrite + Send> = match selector {
            DebugProbeSelector::Usb { .. } => Err(DiscoveryError::InvalidInterfaces)?,
            DebugProbeSelector::SocketAddr(socket_addr) => Box::new(
                TcpStream::connect(*socket_addr).map_err(DiscoveryError::ConnectionFailed)?,
            ),
            #[cfg(target_os = "linux")]
            DebugProbeSelector::UnixSocketAddr(path) => {
                Box::new(UnixStream::connect(path).map_err(DiscoveryError::ConnectionFailed)?)
            }
        };
        tracing::info!("opened Glasgow Interface Explorer ({selector})");
        Ok(Self(stream))
    }

    pub fn transfer(
        &mut self,
        output: Vec<u8>,
        mut input: impl FnMut(Vec<u8>) -> Result<bool, DebugProbeError>,
    ) -> Result<(), DebugProbeError> {
        if !output.is_empty() {
            tracing::trace!("send: {}", hexdump(&output));
            self.0.write(&output).map_err(DebugProbeError::Usb)?;
        }
        let mut buffer = Vec::new();
        while !input(buffer)? {
            buffer = vec![0; 65536];
            let buffer_len = self.0.read(&mut buffer[..]).map_err(DebugProbeError::Usb)?;
            buffer.truncate(buffer_len);
            tracing::trace!("recv: {}", hexdump(&buffer));
        }
        Ok(())
    }
}
