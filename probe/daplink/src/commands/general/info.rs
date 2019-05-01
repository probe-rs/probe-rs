use crate::commands::{
    Response,
    Category,
    Request,
    Error,
    Result,
};

use scroll::Pread;


#[derive(Copy, Clone)]
pub enum Command {
    VendorID = 0x01,
    ProductId = 0x02,
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

pub struct VendorID(String);

impl Response for VendorID {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &VendorID)
    }
}

pub struct ProductID(String);

impl Response for ProductID {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &ProductID)
    }
}

pub struct SerialNumber(String);

impl Response for SerialNumber {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &SerialNumber)
    }
}

pub struct FirmwareVersion(String);

impl Response for FirmwareVersion {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &FirmwareVersion)
    }
}

pub struct TargetDeviceVendor(String);

impl Response for TargetDeviceVendor {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &TargetDeviceVendor)
    }
}

pub struct TargetDeviceName(String);

impl Response for TargetDeviceName {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        string_from_bytes(buffer, offset, &TargetDeviceName)
    }
}

pub struct Capabilities {
    swd_implemented: bool,
    jtag_implemented: bool,
    swo_uart_implemented: bool,
    swo_manchester_implemented: bool,
    atomic_commands_implemented: bool,
    test_domain_timer_implemented: bool,
    swo_streaming_trace_implemented: bool,
}

impl Response for Capabilities {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        // This response can contain two info bytes.
        // In the docs only the first byte is described, so for now we always will only parse that specific byte.
        if buffer[offset + 1] > 0 {
            Ok(Capabilities {
                swd_implemented:                    buffer[offset + 2] & 0x01 > 0,
                jtag_implemented:                   buffer[offset + 2] & 0x02 > 0,
                swo_uart_implemented:               buffer[offset + 2] & 0x04 > 0,
                swo_manchester_implemented:         buffer[offset + 2] & 0x08 > 0,
                atomic_commands_implemented:        buffer[offset + 2] & 0x10 > 0,
                test_domain_timer_implemented:      buffer[offset + 2] & 0x20 > 0,
                swo_streaming_trace_implemented:    buffer[offset + 2] & 0x40 > 0,
            })
        } else {
            Err(Error::UnexpectedAnswer)
        }
    }
}

pub struct TestDomainTime(u32);

impl Response for TestDomainTime {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset + 1] == 0x08 {
            let res = buffer.pread::<u32>(offset + 2).expect("This is a bug. Please report it.");
            Ok(TestDomainTime(res))
        } else {
            Err(Error::UnexpectedAnswer)
        }
    }
}

pub struct SWOTraceBufferSize(u32);

impl Response for SWOTraceBufferSize {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset + 1] == 0x04 {
            let res = buffer.pread::<u32>(offset + 2).expect("This is a bug. Please report it.");
            Ok(SWOTraceBufferSize(res))
        } else {
            Err(Error::UnexpectedAnswer)
        }
    }
}

pub struct PacketCount(u8);

impl Response for PacketCount {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset + 1] == 0x01 {
            let res = buffer.pread::<u8>(offset + 2).expect("This is a bug. Please report it.");
            Ok(PacketCount(res))
        } else {
            Err(Error::UnexpectedAnswer)
        }
    }
}

pub struct PacketSize(u16);

impl Response for PacketSize {
    fn from_bytes(buffer: &[u8], offset: usize) -> Result<Self> {
        if buffer[offset + 1] == 0x02 {
            let res = buffer.pread::<u16>(offset + 2).expect("This is a bug. Please report it.");
            Ok(PacketSize(res))
        } else {
            Err(Error::UnexpectedAnswer)
        }
    }
}

fn string_from_bytes<R, F: Fn(String) -> R>(buffer: &[u8], offset: usize, constructor: &F) -> Result<R> {
    let buffer_end = buffer[offset + 1] as usize + 2;
    let res = std::str::from_utf8(&buffer[offset + 2..offset + buffer_end + 1]).expect("This is a bug. Please report it.");
    Ok(constructor(res.to_owned()))
}