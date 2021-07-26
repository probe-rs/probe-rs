use super::super::{CommandId, Request, SendError};

use scroll::{Pread, LE};

macro_rules! info_command {
    ($id:expr, $name:ident, $response_type:ty) => {
        #[derive(Clone, Default, Debug)]
        pub struct $name {}

        impl Request for $name {
            const COMMAND_ID: CommandId = CommandId::Info;

            type Response = $response_type;

            fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
                buffer[0] = $id;
                Ok(1)
            }

            fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
                ParseFromResponse::from_response(buffer)
            }
        }
    };
}

info_command!(0x01, VendorCommand, Option<String>);

info_command!(0x02, ProductIdCommand, Option<String>);

info_command!(0x03, SerialNumberCommand, Option<String>);

info_command!(0x04, FirmwareVersionCommand, Option<String>);

info_command!(0x05, TargetDeviceVendorCommand, Option<String>);

info_command!(0x06, TargetDeviceNameCommand, Option<String>);

info_command!(0x07, TargetBoardVendorCommand, Option<String>);

info_command!(0x08, TargetBoardNameCommand, Option<String>);

info_command!(0xF0, CapabilitiesCommand, Capabilities);

#[derive(Copy, Clone, Debug)]
pub struct TestDomainTimeCommand {}

impl Request for TestDomainTimeCommand {
    const COMMAND_ID: CommandId = CommandId::Info;

    type Response = u32;

    fn to_bytes(&self, buffer: &mut [u8]) -> Result<usize, SendError> {
        buffer[0] = 0xF1;
        Ok(1)
    }
    fn from_bytes(&self, buffer: &[u8]) -> Result<Self::Response, SendError> {
        if buffer[0] == 0x08 {
            let res = buffer
                .pread_with::<u32>(1, LE)
                .map_err(|_| SendError::NotEnoughData)?;
            Ok(res)
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}

info_command!(0xFE, UartReceiveBufferSizeCommand, u32);
info_command!(0xFC, UartTransmitBufferSizeCommand, u32);
info_command!(0xFD, SWOTraceBufferSizeCommand, u32);
info_command!(0xFE, PacketCountCommand, u8);
info_command!(0xFF, PacketSizeCommand, u16);

trait ParseFromResponse: Sized {
    fn from_response(buffer: &[u8]) -> Result<Self, SendError>;
}

impl ParseFromResponse for Option<String> {
    /// Create a String out of the received buffer.
    ///
    /// The length of the buffer is read from the first byte of the buffer.
    /// If the length is zero, no string is returned.
    fn from_response(buffer: &[u8]) -> Result<Self, SendError> {
        let string_len = buffer[0] as usize; // including the zero terminator

        match string_len {
            0 => Ok(None),
            n => {
                let res = std::str::from_utf8(&buffer[1..1 + n])?;
                Ok(Some(res.to_owned()))
            }
        }
    }
}

impl ParseFromResponse for u8 {
    fn from_response(buffer: &[u8]) -> Result<Self, SendError> {
        if buffer[0] != 1 {
            Err(SendError::UnexpectedAnswer)
        } else {
            Ok(buffer.pread_with(1, LE).unwrap())
        }
    }
}

impl ParseFromResponse for u16 {
    fn from_response(buffer: &[u8]) -> Result<Self, SendError> {
        if buffer[0] != 2 {
            Err(SendError::UnexpectedAnswer)
        } else {
            Ok(buffer.pread_with(1, LE).unwrap())
        }
    }
}

impl ParseFromResponse for u32 {
    fn from_response(buffer: &[u8]) -> Result<Self, SendError> {
        if buffer[0] != 4 {
            Err(SendError::UnexpectedAnswer)
        } else {
            Ok(buffer.pread_with(1, LE).unwrap())
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct Capabilities {
    pub(crate) swd_implemented: bool,
    pub(crate) jtag_implemented: bool,
    pub(crate) swo_uart_implemented: bool,
    pub(crate) swo_manchester_implemented: bool,
    pub(crate) atomic_commands_implemented: bool,
    pub(crate) test_domain_timer_implemented: bool,
    pub(crate) swo_streaming_trace_implemented: bool,
    pub(crate) uart_communication_port_implemented: bool,
    pub(crate) uart_com_port_implemented: bool,
}

impl ParseFromResponse for Capabilities {
    fn from_response(buffer: &[u8]) -> Result<Self, SendError> {
        // This response can contain two info bytes.
        // In the docs only the first byte is described, so for now we always will only parse that specific byte.
        if buffer[0] > 0 {
            let mut capabilites = Capabilities {
                swd_implemented: buffer[1] & 0x01 > 0,
                jtag_implemented: buffer[1] & 0x02 > 0,
                swo_uart_implemented: buffer[1] & 0x04 > 0,
                swo_manchester_implemented: buffer[1] & 0x08 > 0,
                atomic_commands_implemented: buffer[1] & 0x10 > 0,
                test_domain_timer_implemented: buffer[1] & 0x20 > 0,
                swo_streaming_trace_implemented: buffer[1] & 0x40 > 0,
                uart_communication_port_implemented: buffer[1] & 0x80 > 0,
                uart_com_port_implemented: false,
            };

            if buffer[0] >= 2 {
                capabilites.uart_com_port_implemented = buffer[2] & (1 << 0) != 0
            }

            Ok(capabilites)
        } else {
            Err(SendError::UnexpectedAnswer)
        }
    }
}
