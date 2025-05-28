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
        let Some(qual_addr) = selector.serial_number.clone() else {
            Err(ProbeCreationError::NotFound)?
        };
        let stream: Box<dyn ReadWrite + Send> = match *qual_addr.splitn(2, ":").collect::<Vec<_>>()
        {
            ["tcp", addr] => {
                Box::new(TcpStream::connect(addr).map_err(DiscoveryError::ConnectionFailed)?)
            }
            #[cfg(unix)]
            ["unix", addr] => {
                Box::new(UnixStream::connect(addr).map_err(DiscoveryError::ConnectionFailed)?)
            }
            #[cfg(not(unix))]
            ["unix", _addr] => Err(ProbeCreationError::NotFound)?,
            _ => Err(DiscoveryError::InvalidFormat)?,
        };
        tracing::info!("opened Glasgow Interface Explorer ({qual_addr})");
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
