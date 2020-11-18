pub mod commands;
pub mod tools;

use crate::{
    architecture::arm::{
        communication_interface::ArmProbeInterface,
        dp::{Ctrl, DPAccess, DPRegister, DebugPortError},
        swo::poll_interval_from_buf_size,
        ArmCommunicationInterface, DAPAccess, DapError, PortType, Register, SwoAccess, SwoConfig,
        SwoMode,
    },
    probe::{daplink::commands::CmsisDapError, BatchCommand},
    DebugProbe, DebugProbeError, DebugProbeSelector, Error as ProbeRsError, WireProtocol,
};

use commands::{
    general::{
        connect::{ConnectRequest, ConnectResponse},
        disconnect::{DisconnectRequest, DisconnectResponse},
        host_status::{HostStatusRequest, HostStatusResponse},
        info::{Capabilities, Command, PacketCount, PacketSize, SWOTraceBufferSize},
        reset::{ResetRequest, ResetResponse},
    },
    swd,
    swj::{
        clock::{SWJClockRequest, SWJClockResponse},
        pins::{SWJPinsRequestBuilder, SWJPinsResponse},
        sequence::{SequenceRequest, SequenceResponse},
    },
    swo,
    transfer::{
        configure::{ConfigureRequest, ConfigureResponse},
        Ack, InnerTransferRequest, TransferBlockRequest, TransferBlockResponse, TransferRequest,
        TransferResponse, RW,
    },
    DAPLinkDevice, Status,
};

use log::debug;

use std::sync::Mutex;
use std::time::Duration;

use anyhow::anyhow;

pub struct DAPLink {
    pub device: Mutex<DAPLinkDevice>,
    _hw_version: u8,
    _jtag_version: u8,
    protocol: Option<WireProtocol>,

    packet_size: Option<u16>,
    packet_count: Option<u8>,
    capabilities: Option<Capabilities>,
    swo_buffer_size: Option<usize>,
    swo_active: bool,
    swo_streaming: bool,

    /// Speed in kHz
    speed_khz: u32,

    batch: Vec<BatchCommand>,
}

impl std::fmt::Debug for DAPLink {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("DAPLink")
            .field("protocol", &self.protocol)
            .field("packet_size", &self.packet_size)
            .field("packet_count", &self.packet_count)
            .field("capabilities", &self.capabilities)
            .field("swo_buffer_size", &self.swo_buffer_size)
            .field("swo_active", &self.swo_active)
            .field("swo_streaming", &self.swo_streaming)
            .field("speed_khz", &self.speed_khz)
            .finish()
    }
}

impl DAPLink {
    pub fn new_from_device(device: DAPLinkDevice) -> Self {
        // Discard anything left in buffer, as otherwise
        // we'll get out of sync between requests and responses.
        if let DAPLinkDevice::V1(ref hid_device) = device {
            let mut discard_buffer = [0u8; 128];
            loop {
                match hid_device.read_timeout(&mut discard_buffer, 1) {
                    Ok(n) if n != 0 => continue,
                    _ => break,
                }
            }
        }

        Self {
            device: Mutex::new(device),
            _hw_version: 0,
            _jtag_version: 0,
            protocol: None,
            packet_count: None,
            packet_size: None,
            capabilities: None,
            swo_buffer_size: None,
            swo_active: false,
            swo_streaming: false,
            speed_khz: 1_000,
            batch: Vec::new(),
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
            SWJClockResponse(Status::DAPError) => Err(anyhow!(CmsisDapError::ErrorResponse)),
        })?;
        Ok(())
    }

    fn transfer_configure(&mut self, request: ConfigureRequest) -> Result<(), CmsisDapError> {
        commands::send_command::<ConfigureRequest, ConfigureResponse>(&mut self.device, request)
            .and_then(|v| match v {
                ConfigureResponse(Status::DAPOk) => Ok(()),
                ConfigureResponse(Status::DAPError) => Err(anyhow!(CmsisDapError::ErrorResponse)),
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
            swd::configure::ConfigureResponse(Status::DAPError) => Err(anyhow!(CmsisDapError::ErrorResponse)),
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
                SequenceResponse(Status::DAPError) => Err(anyhow!(CmsisDapError::ErrorResponse)),
            })?;
        Ok(())
    }

    /// Immediately send whatever is in our batch if it is not empty.
    ///
    /// This will ensure any pending writes are processed and errors from them
    /// raised if necessary.
    fn process_batch(&mut self) -> Result<u32, DebugProbeError> {
        if self.batch.is_empty() {
            return Ok(0);
        }
        debug!("Processing batch of {} items", self.batch.len());

        let batch = std::mem::replace(&mut self.batch, Vec::new());

        let transfers: Vec<InnerTransferRequest> = batch
            .iter()
            .map(|command| match *command {
                BatchCommand::Read(port, addr) => {
                    InnerTransferRequest::new(port.into(), RW::R, addr as u8, None)
                }
                BatchCommand::Write(port, addr, data) => {
                    InnerTransferRequest::new(port.into(), RW::W, addr as u8, Some(data))
                }
            })
            .collect();

        let response = commands::send_command::<TransferRequest, TransferResponse>(
            &mut self.device,
            TransferRequest::new(&transfers),
        )?;

        let count = response.transfer_count as usize;

        match count {
            _ if count == batch.len() => {
                if response.transfer_response.protocol_error {
                    Err(DapError::SwdProtocol.into())
                } else {
                    match response.transfer_response.ack {
                        Ack::Ok => {
                            log::trace!("ack",);
                            Ok(response.transfer_data)
                        }
                        Ack::NoAck => {
                            log::trace!("nack",);
                            Err(DapError::NoAcknowledge.into())
                        }
                        Ack::Fault => {
                            log::trace!("fault",);

                            let response = DAPAccess::read_register(
                                self,
                                PortType::DebugPort,
                                Ctrl::ADDRESS as u16,
                            )?;
                            let ctrl = Ctrl::from(response);
                            log::trace!(
                                "Writing DAP register failed. Ctrl/Stat register value is: {:?}",
                                ctrl
                            );

                            Err(DapError::FaultResponse.into())
                        }
                        Ack::Wait => {
                            log::trace!("wait",);

                            Err(DapError::WaitResponse.into())
                        }
                    }
                }
            }
            0 => Err(DebugProbeError::Other(anyhow!(
                "Didn't receive any answer during batch processing: {:?}",
                batch
            ))),
            _ => Err(DebugProbeError::BatchError(batch[count - 1])),
        }
    }

    /// Add a BatchCommand to our current batch.
    ///
    /// If the BatchCommand is a Read, this will immediately process the batch
    /// and return the read value. If the BatchCommand is a write, the write is
    /// executed immediately if the batch is full, otherwise it is queued for
    /// later execution.
    fn batch_add(&mut self, command: BatchCommand) -> Result<u32, DebugProbeError> {
        debug!("Adding command to batch: {}", command);

        self.batch.push(command);

        // We always immediately process any reads, which means there will never
        // be more than one read in a batch. We also process whenever the batch
        // is as long as can fit in one packet.
        let max_writes = (self.packet_size.unwrap_or(32) as usize - 3) / (1 + 4);
        match command {
            BatchCommand::Read(_, _) => self.process_batch(),
            _ if self.batch.len() == max_writes => self.process_batch(),
            _ => Ok(0),
        }
    }

    /// Set SWO port to use requested transport.
    ///
    /// Check the probe capabilities to determine which transports are available.
    fn set_swo_transport(
        &mut self,
        transport: swo::TransportRequest,
    ) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, transport)?;
        match response {
            swo::TransportResponse(Status::DAPOk) => Ok(()),
            swo::TransportResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    /// Set SWO port to specified mode.
    ///
    /// Check the probe capabilities to determine which modes are available.
    fn set_swo_mode(&mut self, mode: swo::ModeRequest) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, mode)?;
        match response {
            swo::ModeResponse(Status::DAPOk) => Ok(()),
            swo::ModeResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    /// Set SWO port to specified baud rate.
    ///
    /// Returns `SWOBaudrateNotConfigured` if the probe returns 0,
    /// indicating the requested baud rate was not configured,
    /// and returns the configured baud rate on success (which
    /// may differ from the requested baud rate).
    fn set_swo_baudrate(&mut self, baud: swo::BaudrateRequest) -> Result<u32, DebugProbeError> {
        let response: swo::BaudrateResponse = commands::send_command(&mut self.device, baud)?;
        debug!("Requested baud {}, got {}", baud.0, response.0);
        if response.0 == 0 {
            Err(CmsisDapError::SWOBaudrateNotConfigured.into())
        } else {
            Ok(response.0)
        }
    }

    /// Start SWO trace data capture.
    fn start_swo_capture(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, swo::ControlRequest::Start)?;
        match response {
            swo::ControlResponse(Status::DAPOk) => Ok(()),
            swo::ControlResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    /// Stop SWO trace data capture.
    fn stop_swo_capture(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, swo::ControlRequest::Stop)?;
        match response {
            swo::ControlResponse(Status::DAPOk) => Ok(()),
            swo::ControlResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    /// Fetch current SWO trace status.
    #[allow(dead_code)]
    fn get_swo_status(&mut self) -> Result<swo::StatusResponse, DebugProbeError> {
        Ok(commands::send_command(
            &mut self.device,
            swo::StatusRequest,
        )?)
    }

    /// Fetch extended SWO trace status.
    ///
    /// request.request_status: request trace status
    /// request.request_count: request remaining bytes in trace buffer
    /// request.request_index: request sequence number and timestamp of next trace sequence
    #[allow(dead_code)]
    fn get_swo_extended_status(
        &mut self,
        request: swo::ExtendedStatusRequest,
    ) -> Result<swo::ExtendedStatusResponse, DebugProbeError> {
        Ok(commands::send_command(&mut self.device, request)?)
    }

    /// Fetch latest SWO trace data by sending a DAP_SWO_Data request.
    fn get_swo_data(&mut self) -> Result<Vec<u8>, DebugProbeError> {
        match self.swo_buffer_size {
            Some(swo_buffer_size) => {
                // We'll request the smaller of the probe's SWO buffer and
                // its maximum packet size. If the probe has less data to
                // send it will respond with as much as it can.
                let n = if let Some(packet_size) = self.packet_size {
                    usize::min(swo_buffer_size, packet_size as usize) as u16
                } else {
                    usize::min(swo_buffer_size, u16::MAX as usize) as u16
                };

                let response: swo::DataResponse =
                    commands::send_command(&mut self.device, swo::DataRequest { max_count: n })?;
                if response.status.error {
                    Err(CmsisDapError::SWOTraceStreamError.into())
                } else {
                    Ok(response.data)
                }
            }
            None => Ok(Vec::new()),
        }
    }
}

impl DPAccess for DAPLink {
    fn read_dp_register<R: DPRegister>(&mut self) -> Result<R, DebugPortError> {
        debug!("Reading DP register {}", R::NAME);
        let result = self.read_register(PortType::DebugPort, u16::from(R::ADDRESS))?;

        debug!("Read    DP register {}, value=0x{:08x}", R::NAME, result);

        Ok(result.into())
    }

    fn write_dp_register<R: DPRegister>(&mut self, register: R) -> Result<(), DebugPortError> {
        let value = register.into();

        debug!("Writing DP register {}, value=0x{:08x}", R::NAME, value);
        self.write_register(PortType::DebugPort, u16::from(R::ADDRESS), value)?;

        Ok(())
    }
}

impl DebugProbe for DAPLink {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        Ok(Box::new(Self::new_from_device(
            tools::open_device_from_selector(selector)?,
        )))
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

        let caps = commands::send_command(&mut self.device, Command::Capabilities)?;
        self.capabilities = Some(caps);
        debug!("Detected probe capabilities: {:?}", caps);

        if caps.swo_uart_implemented || caps.swo_manchester_implemented {
            let swo_size: SWOTraceBufferSize =
                commands::send_command(&mut self.device, Command::SWOTraceBufferSize)?;
            self.swo_buffer_size = Some(swo_size.0 as usize);
            debug!("Probe SWO buffer size: {}", swo_size.0);
        }

        debug!("Attaching to target system (clock = {}kHz)", self.speed_khz);

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
            ConnectResponse::InitFailed => Err(anyhow!(CmsisDapError::ErrorResponse)),
        })?;

        // Set speed after connecting as it can be reset during protocol selection
        self.set_speed(self.speed_khz)?;

        self.transfer_configure(ConfigureRequest {
            idle_cycles: 0,
            wait_retry: 80,
            match_retry: 0,
        })?;

        self.configure_swd(swd::configure::ConfigureRequest {})?;

        self.send_swj_sequences(SequenceRequest::new(&[
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ])?)?;

        self.send_swj_sequences(SequenceRequest::new(&[0x9e, 0xe7])?)?;

        self.send_swj_sequences(SequenceRequest::new(&[
            0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
        ])?)?;

        self.send_swj_sequences(SequenceRequest::new(&[0x00])?)?;

        debug!("Successfully changed to SWD.");

        // Tell the probe we are connected so it can turn on an LED.
        let _: Result<HostStatusResponse, _> =
            commands::send_command(&mut self.device, HostStatusRequest::connected(true));

        Ok(())
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        self.process_batch()?;

        if self.swo_active {
            self.disable_swo()
                .map_err(|e| DebugProbeError::ProbeSpecific(e.into()))?;
        }

        let response = commands::send_command(&mut self.device, DisconnectRequest {})?;

        // Tell probe we are disconnected so it can turn off its LED.
        let _: Result<HostStatusResponse, _> =
            commands::send_command(&mut self.device, HostStatusRequest::connected(false));

        match response {
            DisconnectResponse(Status::DAPOk) => Ok(()),
            DisconnectResponse(Status::DAPError) => Err(CmsisDapError::UnexpectedAnswer.into()),
        }
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Jtag => {
                log::warn!(
                    "Support for JTAG protocol is not yet implemented for CMSIS-DAP based probes."
                );
                Err(DebugProbeError::UnsupportedProtocol(WireProtocol::Jtag))
            }
            WireProtocol::Swd => {
                self.protocol = Some(WireProtocol::Swd);
                Ok(())
            }
        }
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        commands::send_command(&mut self.device, ResetRequest).map(|v: ResetResponse| {
            log::info!("Target reset response: {:?}", v);
        })?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        let request = SWJPinsRequestBuilder::new().nreset(false).build();

        commands::send_command(&mut self.device, request).map(|v: SWJPinsResponse| {
            log::info!("Pin response: {:?}", v);
        })?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        let request = SWJPinsRequestBuilder::new().nreset(true).build();

        commands::send_command(&mut self.device, request).map(|v: SWJPinsResponse| {
            log::info!("Pin response: {:?}", v);
        })?;
        Ok(())
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        Some(self as _)
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        Some(self as _)
    }

    fn get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Option<Box<dyn ArmProbeInterface + 'probe>>, DebugProbeError> {
        let interface = ArmCommunicationInterface::new(self)?;

        Ok(Some(Box::new(interface)))
    }

    fn has_arm_interface(&self) -> bool {
        true
    }
}

impl<'a> AsRef<dyn DebugProbe + 'a> for DAPLink {
    fn as_ref(&self) -> &(dyn DebugProbe + 'a) {
        self
    }
}

impl<'a> AsMut<dyn DebugProbe + 'a> for DAPLink {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'a) {
        self
    }
}

impl DAPAccess for DAPLink {
    /// Reads the DAP register on the specified port and address.
    fn read_register(&mut self, port: PortType, addr: u16) -> Result<u32, DebugProbeError> {
        self.batch_add(BatchCommand::Read(port, addr))
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(
        &mut self,
        port: PortType,
        addr: u16,
        value: u32,
    ) -> Result<(), DebugProbeError> {
        self.batch_add(BatchCommand::Write(port, addr, value))
            .map(|_| ())
    }

    fn write_block(
        &mut self,
        port: PortType,
        register_address: u16,
        values: &[u32],
    ) -> Result<(), DebugProbeError> {
        self.process_batch()?;

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

            let resp: TransferBlockResponse =
                commands::send_command(&mut self.device, request).map_err(DebugProbeError::from)?;

            if resp.transfer_response != 1 {
                return Err(CmsisDapError::ErrorResponse.into());
            }
        }

        Ok(())
    }

    fn read_block(
        &mut self,
        port: PortType,
        register_address: u16,
        values: &mut [u32],
    ) -> Result<(), DebugProbeError> {
        self.process_batch()?;

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

            let resp: TransferBlockResponse =
                commands::send_command(&mut self.device, request).map_err(DebugProbeError::from)?;

            if resp.transfer_response != 1 {
                return Err(CmsisDapError::ErrorResponse.into());
            }

            chunk.clone_from_slice(&resp.transfer_data[..]);
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<(), DebugProbeError> {
        self.process_batch()?;
        Ok(())
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}

impl SwoAccess for DAPLink {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ProbeRsError> {
        // We read capabilities on initialisation so it should not be None.
        let caps = self.capabilities.expect("This is a bug. Please report it.");

        // Check requested mode is available in probe capabilities
        match config.mode() {
            SwoMode::UART if !caps.swo_uart_implemented => {
                return Err(DebugProbeError::ProbeSpecific(
                    CmsisDapError::SWOModeNotAvailable.into(),
                )
                .into())
            }
            SwoMode::Manchester if !caps.swo_manchester_implemented => {
                return Err(DebugProbeError::ProbeSpecific(
                    CmsisDapError::SWOModeNotAvailable.into(),
                )
                .into())
            }
            _ => (),
        }

        // Stop any ongoing trace
        self.stop_swo_capture()?;

        // Set transport. If the dedicated endpoint is available and we have opened
        // the probe in V2 mode and it has an SWO endpoint, request that, otherwise
        // request the DAP_SWO_Data polling mode.
        if caps.swo_streaming_trace_implemented
            && self.device.get_mut().unwrap().swo_streaming_supported()
        {
            debug!("Starting SWO capture with WinUSB transport");
            self.set_swo_transport(swo::TransportRequest::WinUsbEndpoint)?;
            self.swo_streaming = true;
        } else {
            debug!("Starting SWO capture with polled transport");
            self.set_swo_transport(swo::TransportRequest::DataCommand)?;
            self.swo_streaming = false;
        }

        // Set mode. We've already checked that the requested mode is listed as supported.
        match config.mode() {
            SwoMode::UART => self.set_swo_mode(swo::ModeRequest::Uart)?,
            SwoMode::Manchester => self.set_swo_mode(swo::ModeRequest::Manchester)?,
        }

        // Set baud rate.
        let baud = self.set_swo_baudrate(swo::BaudrateRequest(config.baud()))?;
        if baud != config.baud() {
            log::warn!(
                "Target SWO baud rate not met: requested {}, got {}",
                config.baud(),
                baud
            );
        }

        self.start_swo_capture()?;

        self.swo_active = true;
        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ProbeRsError> {
        debug!("Stopping SWO capture");
        self.stop_swo_capture()?;
        self.swo_active = false;
        Ok(())
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ProbeRsError> {
        if self.swo_active {
            if self.swo_streaming {
                let device = self
                    .device
                    .get_mut()
                    .expect("This is a bug. Please report it.");
                let mut buffer = vec![0u8; 1024];
                let n = device.read_swo_stream(&mut buffer, timeout)?;
                buffer.truncate(n);
                log::trace!("SWO streaming buffer: {:?}", buffer);
                Ok(buffer)
            } else {
                let data = self.get_swo_data()?;
                log::trace!("SWO polled data: {:?}", data);
                Ok(data)
            }
        } else {
            Ok(Vec::new())
        }
    }

    fn swo_poll_interval_hint(&mut self, config: &SwoConfig) -> Option<std::time::Duration> {
        let caps = self.capabilities.expect("This is a bug. Please report it.");
        if caps.swo_streaming_trace_implemented
            && self.device.get_mut().unwrap().swo_streaming_supported()
        {
            // Streaming reads block waiting for new data so any polling interval is fine
            Some(std::time::Duration::from_secs(0))
        } else {
            match self.swo_buffer_size {
                // Given the buffer size and SWO baud rate we can estimate a poll rate.
                Some(buf_size) => poll_interval_from_buf_size(config, buf_size),

                // If we don't know the buffer size, we can't give a meaningful hint.
                None => None,
            }
        }
    }

    fn swo_buffer_size(&mut self) -> Option<usize> {
        self.swo_buffer_size
    }
}

impl Drop for DAPLink {
    fn drop(&mut self) {
        debug!("Detaching from DAPLink");
        // We ignore the error cases as we can't do much about it anyways.
        let _ = self.process_batch();

        // If SWO is active, disable it before calling detach,
        // which ensures detach won't error on disabling SWO.
        if self.swo_active {
            let _ = self.disable_swo();
        }

        let _ = self.detach();
    }
}
