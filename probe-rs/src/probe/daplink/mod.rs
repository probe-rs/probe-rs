pub mod commands;
pub mod tools;

use crate::architecture::arm::{
    dp::{DPAccess, DPRegister, DebugPort},
    DAPAccess, DapError, PortType,
};
use crate::probe::daplink::commands::CmsisDapError;
use crate::{DebugProbe, DebugProbeError, DebugProbeInfo, Memory, WireProtocol};
use commands::{
    general::{
        connect::{ConnectRequest, ConnectResponse},
        disconnect::{DisconnectRequest, DisconnectResponse},
        info::{Command, PacketCount, PacketSize},
        reset::{ResetRequest, ResetResponse},
    },
    swd,
    swj::{
        clock::{SWJClockRequest, SWJClockResponse},
        sequence::{SequenceRequest, SequenceResponse},
    },
    transfer::{
        configure::{ConfigureRequest, ConfigureResponse},
        Ack, InnerTransferRequest, TransferBlockRequest, TransferBlockResponse, TransferRequest,
        TransferResponse, RW,
    },
    Status,
};
use log::debug;

use super::JTAGAccess;
use std::sync::Mutex;

pub struct DAPLink {
    pub device: Mutex<hidapi::HidDevice>,
    _hw_version: u8,
    _jtag_version: u8,
    protocol: Option<WireProtocol>,

    packet_size: Option<u16>,
    packet_count: Option<u8>,

    /// Speed in kHz
    speed_khz: u32,
}

impl std::fmt::Debug for DAPLink {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("DAPLink")
            .field("device", &"hidapi::HidDevice")
            .field("protocol", &self.protocol)
            .field("packet_size", &self.packet_size)
            .field("packet_count", &self.packet_count)
            .finish()
    }
}

impl DAPLink {
    pub fn new_from_device(device: hidapi::HidDevice) -> Self {
        Self {
            device: Mutex::new(device),
            _hw_version: 0,
            _jtag_version: 0,
            protocol: None,
            packet_count: None,
            packet_size: None,
            speed_khz: 1_000,
        }
    }

    /// Set maximum JTAG/SWD clock frequency to use, in Hz.
    ///
    /// The actual clock frequency used by the device might be lower.
    fn set_swj_clock(&mut self, clock_hz: u32) -> Result<(), CmsisDapError> {
        commands::send_command::<SWJClockRequest, SWJClockResponse>(
            &mut self.device,
            SWJClockRequest(clock_hz),
        )
        .and_then(|v| match v {
            SWJClockResponse(Status::DAPOk) => Ok(()),
            SWJClockResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
        })?;
        Ok(())
    }

    fn transfer_configure(&mut self, request: ConfigureRequest) -> Result<(), CmsisDapError> {
        commands::send_command::<ConfigureRequest, ConfigureResponse>(&mut self.device, request)
            .and_then(|v| match v {
                ConfigureResponse(Status::DAPOk) => Ok(()),
                ConfigureResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })?;
        Ok(())
    }

    fn configure_swd(
        &mut self,
        request: swd::configure::ConfigureRequest,
    ) -> Result<(), CmsisDapError> {
        commands::send_command::<swd::configure::ConfigureRequest, swd::configure::ConfigureResponse>(
            &mut self.device,
            request
        )
        .and_then(|v| match v {
            swd::configure::ConfigureResponse(Status::DAPOk) => Ok(()),
            swd::configure::ConfigureResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
        })?;
        Ok(())
    }

    fn send_swj_sequences(&mut self, request: SequenceRequest) -> Result<(), CmsisDapError> {
        /* 12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence
        12 10 9E E7 -> 12 00 // SWJ Sequence
        12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence */
        //let sequence_1 = SequenceRequest::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);

        commands::send_command::<SequenceRequest, SequenceResponse>(&mut self.device, request)
            .and_then(|v| match v {
                SequenceResponse(Status::DAPOk) => Ok(()),
                SequenceResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })?;
        Ok(())
    }
}

impl DPAccess for DAPLink {
    type Error = DebugProbeError;

    fn read_dp_register<R: DPRegister<P>, P: DebugPort>(&mut self) -> Result<R, Self::Error> {
        debug!("Reading DP register {}", R::NAME);
        let result = self.read_register(PortType::DebugPort, u16::from(R::ADDRESS))?;

        debug!("Read    DP register {}, value=0x{:08x}", R::NAME, result);

        Ok(result.into())
    }

    fn write_dp_register<R: DPRegister<P>, P: DebugPort>(
        &mut self,
        register: R,
    ) -> Result<(), Self::Error> {
        let value = register.into();

        debug!("Writing DP register {}, value=0x{:08x}", R::NAME, value);
        self.write_register(PortType::DebugPort, u16::from(R::ADDRESS), value)
    }
}

impl DebugProbe for DAPLink {
    fn new_from_probe_info(info: &DebugProbeInfo) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        if let Some(serial_number) = &info.serial_number {
            Ok(Box::new(Self::new_from_device(
                hidapi::HidApi::new()
                    .map_err(|_| DebugProbeError::ProbeCouldNotBeCreated)?
                    .open_serial(info.vendor_id, info.product_id, &serial_number)
                    .map_err(|_| DebugProbeError::ProbeCouldNotBeCreated)?,
            )))
        } else {
            Ok(Box::new(Self::new_from_device(
                hidapi::HidApi::new()
                    .map_err(|_| DebugProbeError::ProbeCouldNotBeCreated)?
                    .open(info.vendor_id, info.product_id)
                    .map_err(|_| DebugProbeError::ProbeCouldNotBeCreated)?,
            )))
        }
    }

    fn get_name(&self) -> &str {
        "DAPLink"
    }

    /// Get the currently set maximum speed.
    ///
    /// CMSIS-DAP offers no possibility to get the actual speed used.
    fn speed(&self) -> u32 {
        self.speed_khz
    }

    /// For CMSIS-DAP, we can set the maximum speed. The actual speed
    /// used by the probe cannot be determined, but it will not be
    /// higher than this value.
    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        self.set_swj_clock(speed_khz * 1_000)?;
        self.speed_khz = speed_khz;

        Ok(speed_khz)
    }

    /// Enters debug mode.
    fn attach(&mut self) -> Result<(), DebugProbeError> {
        // get information about the daplink
        let PacketCount(packet_count) =
            commands::send_command(&mut self.device, Command::PacketCount)?;
        let PacketSize(packet_size) =
            commands::send_command(&mut self.device, Command::PacketSize)?;

        self.packet_count = Some(packet_count);
        self.packet_size = Some(packet_size);

        let clock = 1_000_000;

        debug!("Attaching to target system (clock = {})", clock);
        self.set_swj_clock(clock)?;

        let protocol = if let Some(protocol) = self.protocol {
            match protocol {
                WireProtocol::Swd => ConnectRequest::UseSWD,
                WireProtocol::Jtag => ConnectRequest::UseJTAG,
            }
        } else {
            ConnectRequest::UseDefaultPort
        };

        let _result = commands::send_command(&mut self.device, protocol).and_then(|v| match v {
            ConnectResponse::SuccessfulInitForSWD => Ok(WireProtocol::Swd),
            ConnectResponse::SuccessfulInitForJTAG => Ok(WireProtocol::Jtag),
            ConnectResponse::InitFailed => Err(CmsisDapError::ErrorResponse),
        })?;

        self.set_swj_clock(clock)?;

        self.transfer_configure(ConfigureRequest {
            idle_cycles: 0,
            wait_retry: 80,
            match_retry: 0,
        })?;

        self.configure_swd(swd::configure::ConfigureRequest {})?;

        self.send_swj_sequences(
            SequenceRequest::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap(),
        )?;

        self.send_swj_sequences(SequenceRequest::new(&[0x9e, 0xe7]).unwrap())?;

        self.send_swj_sequences(
            SequenceRequest::new(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]).unwrap(),
        )?;

        self.send_swj_sequences(SequenceRequest::new(&[0x00]).unwrap())?;

        debug!("Successfully changed to SDW.");

        Ok(())
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, DisconnectRequest {})?;

        match response {
            DisconnectResponse(Status::DAPOk) => Ok(()),
            DisconnectResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Jtag => self.protocol = Some(WireProtocol::Jtag),
            WireProtocol::Swd => self.protocol = Some(WireProtocol::Swd),
        }
        Ok(())
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        commands::send_command(&mut self.device, ResetRequest).map(|v: ResetResponse| {
            log::info!("Target reset response: {:?}", v);
        })?;
        Ok(())
    }

    fn dedicated_memory_interface(&self) -> Option<Memory> {
        None
    }

    fn get_interface_dap(&self) -> Option<&dyn DAPAccess> {
        Some(self as _)
    }

    fn get_interface_dap_mut(&mut self) -> Option<&mut dyn DAPAccess> {
        Some(self as _)
    }
    fn get_interface_jtag(&self) -> Option<&dyn JTAGAccess> {
        None
    }
    fn get_interface_jtag_mut(&mut self) -> Option<&mut dyn JTAGAccess> {
        None
    }
}

impl DAPAccess for DAPLink {
    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: PortType, addr: u16) -> Result<u32, DebugProbeError> {
        let response = commands::send_command::<TransferRequest, TransferResponse>(
            &mut self.device,
            TransferRequest::new(InnerTransferRequest::new(port.into(), RW::R, addr as u8), 0),
        )?;

        if response.transfer_count == 1 {
            if response.transfer_response.protocol_error {
                // An SWD Protocol Error occured
                Err(DapError::SwdProtocol.into())
            } else {
                match response.transfer_response.ack {
                    Ack::Ok => Ok(response.transfer_data),
                    Ack::NoAck => Err(DapError::NoAcknowledge.into()),
                    Ack::Fault => Err(DapError::FaultResponse.into()),
                    Ack::Wait => Err(DapError::WaitResponse.into()),
                }
            }
        } else {
            Err(CmsisDapError::UnexpectedAnswer.into())
        }
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(
        &mut self,
        port: PortType,
        addr: u16,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        let response = commands::send_command::<TransferRequest, TransferResponse>(
            &mut self.device,
            TransferRequest::new(
                InnerTransferRequest::new(port.into(), RW::W, addr as u8),
                value,
            ),
        )?;

        if response.transfer_count == 1 {
            if response.transfer_response.protocol_error {
                Err(DebugProbeError::USB(None))
            } else {
                match response.transfer_response.ack {
                    Ack::Ok => Ok(()),
                    _ => Err(DebugProbeError::Unknown),
                }
            }
        } else {
            Err(DebugProbeError::Unknown)
        }
    }

    fn write_block(
        &mut self,
        port: PortType,
        register_address: u16,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        // the overhead for a single packet is 6 bytes
        //
        // [0]: HID overhead
        // [1]: Category
        // [2]: DAP Index
        // [3]: Len 1
        // [4]: Len 2
        // [5]: Request type
        //

        let max_packet_size_words = (self.packet_size.unwrap_or(32) - 6) / 4;

        let data_chunk_len = max_packet_size_words as usize;

        for (i, chunk) in values.chunks(data_chunk_len).enumerate() {
            let request = TransferBlockRequest::write_request(
                register_address as u8,
                port.into(),
                Vec::from(chunk),
            );

            debug!("Transfer block: chunk={}, len={} bytes", i, chunk.len() * 4);

            let resp: TransferBlockResponse = commands::send_command(&mut self.device, request)
                .map_err(|_| DebugProbeError::Unknown)?;

            assert_eq!(resp.transfer_response, 1);
        }

        Ok(())
    }

    fn read_block(
        &mut self,
        port: PortType,
        register_address: u16,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        // the overhead for a single packet is 6 bytes
        //
        // [0]: HID overhead
        // [1]: Category
        // [2]: DAP Index
        // [3]: Len 1
        // [4]: Len 2
        // [5]: Request type
        //

        let max_packet_size_words = (self.packet_size.unwrap_or(32) - 6) / 4;

        let data_chunk_len = max_packet_size_words as usize;

        for (i, chunk) in values.chunks_mut(data_chunk_len).enumerate() {
            let request = TransferBlockRequest::read_request(
                register_address as u8,
                port.into(),
                chunk.len() as u16,
            );

            debug!("Transfer block: chunk={}, len={} bytes", i, chunk.len() * 4);

            let resp: TransferBlockResponse = commands::send_command(&mut self.device, request)
                .map_err(|_| DebugProbeError::Unknown)?;

            assert_eq!(resp.transfer_response, 1);

            chunk.clone_from_slice(&resp.transfer_data[..]);
        }

        Ok(())
    }
}

impl Drop for DAPLink {
    fn drop(&mut self) {
        debug!("Detaching from DAPLink");
        // We ignore the error case as we can't do much about it anyways.
        let _ = self.detach();
    }
}
