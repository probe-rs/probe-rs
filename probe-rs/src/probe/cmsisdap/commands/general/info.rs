use super::super::{Category, Request, SendError};

use scroll::{Pread, LE};

#[derive(Clone, Default, Debug)]
struct VendorCommand {}

impl Request for VendorCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = VendorID;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x01;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &VendorID)
    }
}

#[derive(Clone, Default, Debug)]
pub struct VendorID(pub(crate) String);

#[derive(Clone, Default, Debug)]
struct ProductIdCommand {}

#[derive(Clone, Default, Debug)]
pub struct ProductID(pub(crate) String);

impl Request for ProductIdCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = ProductID;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x02;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &ProductID)
    }
}

#[derive(Clone, Default, Debug)]
struct SerialNumberCommand {}

#[derive(Clone, Default, Debug)]
pub struct SerialNumber(pub(crate) String);

impl Request for SerialNumberCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = SerialNumber;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x03;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &SerialNumber)
    }
}

#[derive(Clone, Default, Debug)]
struct FirmwareVersionCommand {}

#[derive(Clone, Default, Debug)]
pub struct FirmwareVersion(pub(crate) String);

impl Request for FirmwareVersionCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = FirmwareVersion;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x04;
        Ok(1)
    }

    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &FirmwareVersion)
    }
}

#[derive(Clone, Default, Debug)]
struct TargetDeviceVendorCommand {}

#[derive(Clone, Default, Debug)]
pub struct TargetDeviceVendor(pub(crate) String);

impl Request for TargetDeviceVendorCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = TargetDeviceVendor;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x05;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &TargetDeviceVendor)
    }
}

#[derive(Clone, Default, Debug)]
struct TargetDeviceNameCommand {}

#[derive(Clone, Default, Debug)]
pub struct TargetDeviceName(pub(crate) String);

impl Request for TargetDeviceNameCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = TargetDeviceName;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0x06;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        string_from_bytes(buffer, 0, &TargetDeviceName)
    }
}

#[derive(Copy, Clone, Debug)]
pub struct CapabilitiesCommand {}

#[derive(Copy, Clone, Debug)]
pub struct Capabilities {
    pub(crate) swd_implemented: bool,
    pub(crate) jtag_implemented: bool,
    pub(crate) swo_uart_implemented: bool,
    pub(crate) swo_manchester_implemented: bool,
    pub(crate) atomic_commands_implemented: bool,
    pub(crate) test_domain_timer_implemented: bool,
    pub(crate) swo_streaming_trace_implemented: bool,
}

impl Request for CapabilitiesCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = Capabilities;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xF0;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        // This response can contain two info bytes.
        // In the docs only the first byte is described, so for now we always will only parse that specific byte.
        if buffer[0] > 0 {
            Ok(Capabilities {
                swd_implemented: buffer[1] & 0x01 > 0,
                jtag_implemented: buffer[1] & 0x02 > 0,
                swo_uart_implemented: buffer[1] & 0x04 > 0,
                swo_manchester_implemented: buffer[1] & 0x08 > 0,
                atomic_commands_implemented: buffer[1] & 0x10 > 0,
                test_domain_timer_implemented: buffer[1] & 0x20 > 0,
                swo_streaming_trace_implemented: buffer[1] & 0x40 > 0,
            })
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct TestDomainTimeCommand {}

#[derive(Copy, Clone, Debug)]
pub struct TestDomainTime(pub(crate) u32);

impl Request for TestDomainTimeCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = TestDomainTime;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xF1;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer[0] == 0x08 {
            let res = buffer.pread_with::<u32>(1, LE).unwrap();
            Ok(TestDomainTime(res))
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct SWOTraceBufferSizeCommand {}

#[derive(Copy, Clone, Debug)]
pub struct SWOTraceBufferSize(pub(crate) u32);

impl Request for SWOTraceBufferSizeCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = SWOTraceBufferSize;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xFD;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer[0] == 0x04 {
            let res = buffer.pread_with::<u32>(1, LE).unwrap();
            Ok(SWOTraceBufferSize(res))
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct PacketCountCommand {}

#[derive(Copy, Clone, Debug)]
pub struct PacketCount(pub(crate) u8);

impl Request for PacketCountCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = PacketCount;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xFE;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer[0] == 0x01 {
            let res = buffer.pread_with::<u8>(1, LE).unwrap();
            Ok(PacketCount(res))
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct PacketSizeCommand {}

#[derive(Copy, Clone, Debug)]
pub struct PacketSize(pub(crate) u16);

impl Request for PacketSizeCommand {
    const CATEGORY: Category = Category(0x00);

    type Response = PacketSize;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xFF;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer[0] == 0x02 {
            let res = buffer.pread_with::<u16>(1, LE).unwrap();
            Ok(PacketSize(res))
        } else {
            Err(SendError::UnexpectedAnswer)
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
) -> Result<R, SendError> {
    let string_len = buffer[dbg!(offset)] as usize; // including the zero terminator

    let string_start = offset + 1;
    let string_end = string_start + string_len;

    let res = std::str::from_utf8(&buffer[string_start..string_end])
        .expect("Unable to parse received string.");
    Ok(constructor(res.to_owned()))
}
