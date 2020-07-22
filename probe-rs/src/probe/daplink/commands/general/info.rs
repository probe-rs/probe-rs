use super::super::{Category, CmsisDapError, Request, Response, Result};

use anyhow::anyhow;
use scroll::{Pread, LE};

#[allow(unused)]
#[derive(Copy, Clone, Debug)]
pub enum Command {
    VendorID = 0x01,
    ProductID = 0x02,
    SerialNumber = 0x03,
    FirmwareVersion = 0x04,
    TargetDeviceVendor = 0x05,
    TargetDeviceName = 0x06,
    Capabilities = 0xF0,
    TestDomainTimerParameter = 0xF1,
    SWOTraceBufferSize = 0xFD,
    PacketCount = 0xFE,
    PacketSize = 0xFF,
}

impl Request for Command {
    const CATEGORY: Category = Category(0x00);

    fn to_bytes(&self, buffer: &mut [u8], offset: usize) -> Result<usize> {
        buffer[offset] = *self as u8;
        Ok(1)
    }
}

#[derive(Clone, Default, Debug)]
pub struct VendorID(pub String);

impl Response for VendorID {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &VendorID)
    }
}

#[derive(Clone, Default, Debug)]
pub struct ProductID(pub String);

impl Response for ProductID {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &ProductID)
    }
}

#[derive(Clone, Default, Debug)]
pub struct SerialNumber(pub String);

impl Response for SerialNumber {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &SerialNumber)
    }
}

#[derive(Clone, Default, Debug)]
pub struct FirmwareVersion(pub String);

impl Response for FirmwareVersion {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &FirmwareVersion)
    }
}

#[derive(Clone, Default, Debug)]
pub struct TargetDeviceVendor(pub String);

impl Response for TargetDeviceVendor {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &TargetDeviceVendor)
    }
}

#[derive(Clone, Default, Debug)]
pub struct TargetDeviceName(pub String);

impl Response for TargetDeviceName {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &TargetDeviceName)
    }
}

#[derive(Clone, Debug)]
pub struct Capabilities {
    pub swd_implemented: bool,
    pub jtag_implemented: bool,
    pub swo_uart_implemented: bool,
    pub swo_manchester_implemented: bool,
    pub atomic_commands_implemented: bool,
    pub test_domain_timer_implemented: bool,
    pub swo_streaming_trace_implemented: bool,
}

impl Response for Capabilities {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        // This response can contain two info bytes.
        // In the docs only the first byte is described, so for now we always will only parse that specific byte.
        if buffer[offset] > 0 {
            Ok(Capabilities {
                swd_implemented: buffer[offset + 1] & 0x01 > 0,
                jtag_implemented: buffer[offset + 1] & 0x02 > 0,
                swo_uart_implemented: buffer[offset + 1] & 0x04 > 0,
                swo_manchester_implemented: buffer[offset + 1] & 0x08 > 0,
                atomic_commands_implemented: buffer[offset + 1] & 0x10 > 0,
                test_domain_timer_implemented: buffer[offset + 1] & 0x20 > 0,
                swo_streaming_trace_implemented: buffer[offset + 1] & 0x40 > 0,
            })
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
        }
    }
}

#[derive(Clone, Debug)]
pub struct TestDomainTime(u32);

impl Response for TestDomainTime {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset] == 0x08 {
            let res = buffer
                .pread_with::<u32>(offset + 1, LE)
                .map_err(|_| anyhow!("This is a bug. Please report it."))?;
            Ok(TestDomainTime(res))
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
        }
    }
}

#[derive(Clone, Debug)]
pub struct SWOTraceBufferSize(u32);

impl Response for SWOTraceBufferSize {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset] == 0x04 {
            let res = buffer
                .pread_with::<u32>(offset + 1, LE)
                .map_err(|_| anyhow!("This is a bug. Please report it."))?;
            Ok(SWOTraceBufferSize(res))
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
        }
    }
}

#[derive(Debug)]
pub struct PacketCount(pub u8);

impl Response for PacketCount {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset] == 0x01 {
            let res = buffer
                .pread_with::<u8>(offset + 1, LE)
                .map_err(|_| anyhow!("This is a bug. Please report it."))?;
            Ok(PacketCount(res))
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
        }
    }
}

#[derive(Debug)]
pub struct PacketSize(pub u16);

impl Response for PacketSize {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset] == 0x02 {
            let res = buffer
                .pread_with::<u16>(offset + 1, LE)
                .map_err(|_| anyhow!("This is a bug. Please report it."))?;
            Ok(PacketSize(res))
        } else {
            Err(anyhow!(CmsisDapError::UnexpectedAnswer))
        }
    }
}

/// Create a String out of the received buffer.
///
/// The length of the buffer is read from the buffer, at index offset.
///
fn string_from_bytes<R, F: Fn(String) -> R>(
    buffer: &[u8],
    offset: usize,
    constructor: &F,
) -> Result<R> {
    let string_len = buffer[dbg!(offset)] as usize; // including the zero terminator

    let string_start = offset + 1;
    let string_end = string_start + string_len;

    let res = std::str::from_utf8(&buffer[string_start..string_end])
        .map_err(|_| anyhow!("This is a bug. Please report it."))?;
    Ok(constructor(res.to_owned()))
}
