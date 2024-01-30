//! SWO capture support for J-Link probes.

#![allow(unused)]

use super::Command;
use super::JLink;

use super::capabilities::Capability;
use super::error::JlinkError;
use super::interface::Interface;

use bitflags::bitflags;
use byteorder::{LittleEndian, ReadBytesExt};

use std::{cmp, ops::Deref};
use tracing::warn;

type Result<T> = std::result::Result<T, JlinkError>;

#[repr(u8)]
enum SwoCommand {
    Start = 0x64,
    Stop = 0x65,
    Read = 0x66,
    GetSpeeds = 0x6E,
}

#[repr(u8)]
enum SwoParam {
    Mode = 0x01,
    Baudrate = 0x02,
    ReadSize = 0x03,
    BufferSize = 0x04,
    // FIXME: Do these have hardware/firmware version requirements to be recognized?
}

/// The supported SWO data encoding modes.
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
#[repr(u32)]
#[non_exhaustive]
pub enum SwoMode {
    /// UART mode.
    Uart = 0x00000000,
    // FIXME: Manchester encoding?
}

bitflags! {
    /// SWO status returned by probe on SWO buffer read.
    #[derive(Copy, Clone, Debug, Eq, PartialEq)]
    struct SwoStatus: u32 {
        /// The on-probe buffer has overflowed. Device data was lost.
        const OVERRUN = 1 << 0;
    }
}

impl SwoStatus {
    fn new(bits: u32) -> Self {
        let flags = SwoStatus::from_bits_truncate(bits);
        if flags.bits() != bits {
            warn!("Unknown SWO status flag bits: 0x{:08X}", bits);
        }
        flags
    }
}

/// SWO data that was read via [`super::JLink::swo_read`].
#[derive(Debug)]
pub struct SwoData<'a> {
    data: &'a [u8],
    status: SwoStatus,
}

impl<'a> SwoData<'a> {
    /// Returns whether the probe-internal buffer overflowed before the last read.
    ///
    /// This indicates that some device data was lost.
    pub fn did_overrun(&self) -> bool {
        self.status.contains(SwoStatus::OVERRUN)
    }
}

impl<'a> AsRef<[u8]> for SwoData<'a> {
    fn as_ref(&self) -> &[u8] {
        self.data
    }
}

impl<'a> Deref for SwoData<'a> {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        self.data
    }
}

/// Supported SWO capture speed info.
#[derive(Debug)]
pub struct SwoSpeedInfo {
    base_freq: u32,
    min_div: u32,
    #[allow(dead_code)]
    max_div: u32,

    min_presc: u32,
    #[allow(dead_code)]
    max_presc: u32,
}

impl SwoSpeedInfo {
    /// Returns the maximum supported speed for SWO capture (in Hz).
    pub fn max_speed_hz(&self) -> u32 {
        self.base_freq / self.min_div / cmp::max(1, self.min_presc)
    }
}

impl JLink {
    /// Reads the probe's SWO capture speed information.
    ///
    /// This requires the probe to support [`Capability::Swo`].
    pub fn read_swo_speeds(&self, mode: SwoMode) -> Result<SwoSpeedInfo> {
        self.require_capability(Capability::Swo)?;

        let mut buf = [0; 9];
        buf[0] = Command::Swo as u8;
        buf[1] = SwoCommand::GetSpeeds as u8;
        buf[2] = 0x04; // Next param has 4 data Bytes
        buf[3] = SwoParam::Mode as u8;
        buf[4..8].copy_from_slice(&(mode as u32).to_le_bytes());
        buf[8] = 0x00;

        self.write_cmd(&buf)?;

        let mut buf = [0; 28];
        self.read(&mut buf)?;

        let mut len = [0; 4];
        len.copy_from_slice(&buf[0..4]);
        let len = u32::from_le_bytes(len);
        if len != 28 {
            return Err(JlinkError::Other(format!(
                "Unexpected response length {}, expected 28",
                len
            )));
        }

        // Skip length and reserved word.
        // FIXME: What's the word after the length for?
        let mut buf = &buf[8..];

        Ok(SwoSpeedInfo {
            base_freq: buf.read_u32::<LittleEndian>().unwrap(),
            min_div: buf.read_u32::<LittleEndian>().unwrap(),
            max_div: buf.read_u32::<LittleEndian>().unwrap(),
            min_presc: buf.read_u32::<LittleEndian>().unwrap(),
            max_presc: buf.read_u32::<LittleEndian>().unwrap(),
        })
    }

    /// Starts capturing SWO data.
    ///
    /// This will switch the probe to SWD interface mode if necessary (required for SWO capture).
    ///
    /// Requires the probe to support [`Capability::Swo`].
    ///
    /// # Parameters
    ///
    /// - `mode`: The SWO data encoding mode to use.
    /// - `speed`: The data rate to capture at (when using [`SwoMode::Uart`], this is the UART baud
    ///   rate).
    /// - `buf_size`: The size (in Bytes) of the on-device buffer to allocate for the SWO data.
    pub fn swo_start(&mut self, mode: SwoMode, speed: u32, buf_size: u32) -> Result<()> {
        self.require_capability(Capability::Swo)?;

        // The probe must be in SWD mode for SWO capture to work.
        self.require_interface_selected(Interface::Swd)?;

        let mut buf = [0; 21];
        buf[0] = Command::Swo as u8;
        buf[1] = SwoCommand::Start as u8;
        buf[2] = 0x04;
        buf[3] = SwoParam::Mode as u8;
        buf[4..8].copy_from_slice(&(mode as u32).to_le_bytes());
        buf[8] = 0x04;
        buf[9] = SwoParam::Baudrate as u8;
        buf[10..14].copy_from_slice(&speed.to_le_bytes());
        buf[14] = 0x04;
        buf[15] = SwoParam::BufferSize as u8;
        buf[16..20].copy_from_slice(&buf_size.to_le_bytes());
        buf[20] = 0x00;

        self.write_cmd(&buf)?;

        let mut status = [0; 4];
        self.read(&mut status)?;
        let _status = SwoStatus::new(u32::from_le_bytes(status));

        Ok(())
    }

    /// Stops capturing SWO data.
    pub fn swo_stop(&mut self) -> Result<()> {
        self.require_capability(Capability::Swo)?;

        let buf = [
            Command::Swo as u8,
            SwoCommand::Stop as u8,
            0x00, // no parameters
        ];

        self.write_cmd(&buf)?;

        let mut status = [0; 4];
        self.read(&mut status)?;
        let _status = SwoStatus::new(u32::from_le_bytes(status));
        // FIXME: What to do with the status?

        Ok(())
    }

    /// Reads captured SWO data from the probe and writes it to `data`.
    ///
    /// This needs to be called regularly after SWO capturing has been started. If it is not called
    /// often enough, the buffer on the probe will fill up and device data will be dropped. You can
    /// call [`SwoData::did_overrun`] to check for this condition.
    ///
    /// **Note**: the probe firmware seems to dislike many short SWO reads (as in, the probe will
    /// *fall off the bus and reset*), so it is recommended to use a buffer that is the same size as
    /// the on-probe data buffer.
    pub fn swo_read<'a>(&self, data: &'a mut [u8]) -> Result<SwoData<'a>> {
        let mut cmd = [0; 9];
        cmd[0] = Command::Swo as u8;
        cmd[1] = SwoCommand::Read as u8;
        cmd[2] = 0x04;
        cmd[3] = SwoParam::ReadSize as u8;
        cmd[4..8].copy_from_slice(&(data.len() as u32).to_le_bytes());
        cmd[8] = 0x00;

        self.write_cmd(&cmd)?;

        let mut header = [0; 8];
        self.read(&mut header)?;

        let status = {
            let mut status = [0; 4];
            status.copy_from_slice(&header[0..4]);
            let bits = u32::from_le_bytes(status);
            SwoStatus::new(bits)
        };
        let length = {
            let mut length = [0; 4];
            length.copy_from_slice(&header[4..8]);
            u32::from_le_bytes(length)
        };

        if status.contains(SwoStatus::OVERRUN) {
            warn!("SWO probe buffer overrun");
        }

        let len = length as usize;
        let buf = &mut data[..len];
        self.read(buf)?;

        Ok(SwoData { data: buf, status })
    }
}
