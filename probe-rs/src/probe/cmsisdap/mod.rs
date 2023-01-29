pub mod commands;
pub mod tools;

use crate::{
    architecture::arm::{
        communication_interface::DapProbe,
        communication_interface::UninitializedArmProbe,
        dp::{Abort, Ctrl},
        swo::poll_interval_from_buf_size,
        ArmCommunicationInterface, ArmError, DapError, DpAddress, Pins, PortType, RawDapAccess,
        Register, SwoAccess, SwoConfig, SwoMode,
    },
    probe::{
        cmsisdap::commands::{
            general::info::{CapabilitiesCommand, PacketCountCommand, SWOTraceBufferSizeCommand},
            CmsisDapError,
        },
        BatchCommand, JtagWriteCommand,
    },
    DebugProbe, DebugProbeError, DebugProbeSelector, WireProtocol,
};

use commands::{
    general::{
        connect::{ConnectRequest, ConnectResponse},
        disconnect::{DisconnectRequest, DisconnectResponse},
        host_status::{HostStatusRequest, HostStatusResponse},
        info::Capabilities,
        reset::{ResetRequest, ResetResponse},
    },
    jtag::sequence::{
        Sequence as JtagSequence, SequenceRequest as JtagSequenceRequest,
        SequenceResponse as JtagSequenceResponse,
    },
    swd,
    swj::{
        clock::{SWJClockRequest, SWJClockResponse},
        pins::{SWJPinsRequest, SWJPinsRequestBuilder, SWJPinsResponse},
        sequence::{SequenceRequest, SequenceResponse},
    },
    swo,
    transfer::{
        configure::{ConfigureRequest, ConfigureResponse},
        Ack, InnerTransferRequest, TransferBlockRequest, TransferBlockResponse, TransferRequest,
        RW,
    },
    CmsisDapDevice, Status,
};

use std::{result::Result, time::Duration};

use self::commands::jtag;

pub struct CmsisDap {
    pub device: CmsisDapDevice,
    _hw_version: u8,
    _jtag_version: u8,
    protocol: Option<WireProtocol>,

    packet_size: u16,
    packet_count: u8,
    capabilities: Capabilities,
    swo_buffer_size: Option<usize>,
    swo_active: bool,
    swo_streaming: bool,
    connected: bool,

    /// Speed in kHz
    speed_khz: u32,

    batch: Vec<BatchCommand>,
}

#[derive(Debug, Clone)]
struct JtagChainItem {
    idcode: u32,
    irlen: usize,
}

impl std::fmt::Debug for CmsisDap {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("CmsisDap")
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

impl CmsisDap {
    pub fn new_from_device(mut device: CmsisDapDevice) -> Result<Self, DebugProbeError> {
        // Discard anything left in buffer, as otherwise
        // we'll get out of sync between requests and responses.
        device.drain();

        // Determine and set the packet size. We do this as soon as possible after
        // opening the probe to ensure all future communication uses the correct size.
        let packet_size = device.find_packet_size()? as u16;

        // Read remaining probe information.
        let packet_count = commands::send_command(&mut device, PacketCountCommand {})?;
        let caps: Capabilities = commands::send_command(&mut device, CapabilitiesCommand {})?;
        tracing::debug!("Detected probe capabilities: {:?}", caps);
        let mut swo_buffer_size = None;
        if caps.swo_uart_implemented || caps.swo_manchester_implemented {
            let swo_size = commands::send_command(&mut device, SWOTraceBufferSizeCommand {})?;
            swo_buffer_size = Some(swo_size as usize);
            tracing::debug!("Probe SWO buffer size: {}", swo_size);
        }

        Ok(Self {
            device,
            _hw_version: 0,
            _jtag_version: 0,
            protocol: None,
            packet_count,
            packet_size,
            capabilities: caps,
            swo_buffer_size,
            swo_active: false,
            swo_streaming: false,
            connected: false,
            speed_khz: 1_000,
            batch: Vec::new(),
        })
    }

    /// Set maximum JTAG/SWD clock frequency to use, in Hz.
    ///
    /// The actual clock frequency used by the device might be lower.
    fn set_swj_clock(&mut self, clock_hz: u32) -> Result<(), CmsisDapError> {
        commands::send_command::<SWJClockRequest>(&mut self.device, SWJClockRequest(clock_hz))
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                SWJClockResponse(Status::DAPOk) => Ok(()),
                SWJClockResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn transfer_configure(&mut self, request: ConfigureRequest) -> Result<(), CmsisDapError> {
        commands::send_command::<ConfigureRequest>(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                ConfigureResponse(Status::DAPOk) => Ok(()),
                ConfigureResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn configure_swd(
        &mut self,
        request: swd::configure::ConfigureRequest,
    ) -> Result<(), CmsisDapError> {
        commands::send_command::<swd::configure::ConfigureRequest>(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                swd::configure::ConfigureResponse(Status::DAPOk) => Ok(()),
                swd::configure::ConfigureResponse(Status::DAPError) => {
                    Err(CmsisDapError::ErrorResponse)
                }
            })
    }

    /// Reset JTAG state machine to Test-Logic-Reset.
    fn jtag_ensure_test_logic_reset(&mut self) -> Result<(), CmsisDapError> {
        // let tdi_bytes = 0x3Fu64.to_le_bytes();
        let tdi_bytes = 0u64.to_le_bytes();
        const TMS_HIGH: bool = true;
        const NO_CAPTURE: bool = false;
        let sequence = JtagSequence::new(6, NO_CAPTURE, TMS_HIGH, tdi_bytes)?;
        let sequences = vec![sequence];

        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    /// Reset JTAG state machine to Run-Test/Idle, as requisite precondition for DAP_Transfer commands.
    fn jtag_ensure_run_test_idle(&mut self) -> Result<(), CmsisDapError> {
        // These could be coalesced into one sequence request, but for now we'll keep things simple.

        // First reach Test-Logic-Reset
        self.jtag_ensure_test_logic_reset()?;

        // Then transition to Run-Test-Idle
        const NO_CAPTURE: bool = false;
        const TMS_LOW: bool = false;
        const TDI_ZEROES: [u8; 8] = [0x00; 8];
        let sequence = JtagSequence::new(1, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?;
        let sequences = vec![sequence];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    fn jtag_scan(&mut self) -> Result<Vec<JtagChainItem>, CmsisDapError> {
        let num_targets = self.jtag_detect_num_targets()?;
        let mut targets = vec![
            JtagChainItem {
                idcode: 0,
                irlen: 0
            };
            num_targets
        ];

        if targets.len() == 0 {
            return Ok(targets);
        }

        const CAPTURE: bool = true;
        const NO_CAPTURE: bool = false;
        const TDI_ZEROES: [u8; 8] = [0x00; 8];
        const TDI_ONES: [u8; 8] = [!0x00; 8];
        const TMS_LOW: bool = false;
        const TMS_HIGH: bool = true;

        // Transition to Test-Logic-Reset.
        // This will set IR to IDCODE instruction for all connected devices,
        // except for those that don't support it (in which case they will set IR to BYPASS).
        //
        // TODO: handle the BYPASS scenario.
        // In this case, we can take advantage of the fact that by spec all IDCODEs have 1 as
        // the least-significant-bit; because entering BYPASS implies an initial 0 in its DR,
        // they can be discerned from IDCODE devices when shifting out.
        self.jtag_ensure_test_logic_reset()?;

        // Transition to Shift-DR
        let sequences = vec![
            JtagSequence::new(1, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
            JtagSequence::new(1, NO_CAPTURE, TMS_HIGH, TDI_ZEROES)?,
            JtagSequence::new(2, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        // Gather all IDCODEs
        for target in targets.iter_mut() {
            let sequences = vec![JtagSequence::new(32, CAPTURE, TMS_LOW, TDI_ZEROES)?];
            let idcode = self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;
            target.idcode = u32::from_le_bytes(idcode.try_into().unwrap());
        }

        // Transition to Shift-IR
        let sequences = vec![
            JtagSequence::new(4, NO_CAPTURE, TMS_HIGH, TDI_ZEROES)?,
            JtagSequence::new(2, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        // Now we try to determine the IR length of each device.
        if targets.len() == 1 {
            // Assume IR len will be at most 16 bits.
            let sequences = vec![
                JtagSequence::new(16, NO_CAPTURE, TMS_LOW, TDI_ONES)?,
                JtagSequence::new(16, CAPTURE, TMS_LOW, TDI_ZEROES)?,
            ];
            let tdo = self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;
            let bits = u16::from_le_bytes(tdo.try_into().unwrap());
            targets[0].irlen = bits.trailing_zeros() as usize;
        } else {
            // When going through Capture-IR, many devices will fill the IR scan chain
            // with all zeros, save for the LSB which will be set to 1
            // (e.g. 0b0001).
            //
            // For ARM, see section B3.2.3 The Debug TAP State Machine (DBGTAPSM) of
            // "Arm® Debug Interface Architecture Specification ADIv5.0 to ADIv5.2"
            //
            // We will assume this is the case for all devices.
            // We will also assume the IR len will be at most 16 bits.
            let mut r = Vec::with_capacity(2 * targets.len());

            // Shift out IR in 128 bit chunks
            for _ in 0..(r.capacity() + 15) / 16 {
                let sequences = vec![
                    JtagSequence::new(64, CAPTURE, TMS_LOW, TDI_ONES)?,
                    JtagSequence::new(64, CAPTURE, TMS_LOW, TDI_ONES)?,
                ];
                r.extend(self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?);
            }

            // Stolen from the ftdi module
            let mut ir: u32 = 0;
            let mut irbits: u32 = 0;
            for (i, target) in targets.iter_mut().enumerate() {
                if (!r.is_empty()) && irbits < 8 {
                    let byte = r[0];
                    r.remove(0);
                    ir |= (byte as u32) << irbits;
                    irbits += 8;
                }
                if ir & 0b11 == 0b01 {
                    ir &= !1;
                    let irlen = ir.trailing_zeros();
                    ir >>= irlen;
                    irbits -= irlen;
                    tracing::debug!("tap {} irlen: {}", i, irlen);
                    target.irlen = irlen as usize;
                } else {
                    tracing::debug!("invalid irlen for tap {}", i);
                    // TODO: what error to return here?
                    return Err(CmsisDapError::ErrorResponse);
                }
            }
        }

        todo!()
    }

    /// Detect number of connected TAPs in chain.
    fn jtag_detect_num_targets(&mut self) -> Result<usize, CmsisDapError> {
        const TMS_LOW: bool = false;
        const TMS_HIGH: bool = true;
        const CAPTURE: bool = true;
        const NO_CAPTURE: bool = false;
        const TDI_ZEROES: [u8; 8] = [0x00; 8];
        const TDI_ONES: [u8; 8] = [!0x00; 8];

        // Start at Test-Logic-Reset
        self.jtag_ensure_test_logic_reset()?;

        // Transition to Shift-IR
        let sequences = vec![
            JtagSequence::new(1, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
            JtagSequence::new(2, NO_CAPTURE, TMS_HIGH, TDI_ZEROES)?,
            JtagSequence::new(2, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        // Shift 1280 ones into the IR register(s), 128 bits at a time.
        // This ensures the device(s) are in BYPASS.
        let sequences = vec![JtagSequence::new(64, NO_CAPTURE, TMS_LOW, TDI_ONES)?];
        for _ in 0..10 {
            self.send_jtag_sequences(JtagSequenceRequest::new(sequences.clone())?)?;
        }

        // Transition to Shift-DR (we're still in Shift-IR, so need TDI=1 on first cycle)
        let sequences = vec![
            JtagSequence::new(3, NO_CAPTURE, TMS_HIGH, 1u64.to_le_bytes())?,
            JtagSequence::new(2, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        // Flush DR register(s) with zeroes.
        // As DAP_Transfer only supports targeting 256 devices (the DAP index param is a single byte),
        // we'll make the simplifying assumption that (at most) we will need to shift in 256 ones.
        let sequences = vec![
            JtagSequence::new(64, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
            JtagSequence::new(64, NO_CAPTURE, TMS_LOW, TDI_ZEROES)?,
        ];
        for _ in 0..2 {
            self.send_jtag_sequences(JtagSequenceRequest::new(sequences.clone())?)?;
        }

        // Now shift in ones until we get a one out of TDO
        let sequences = vec![
            JtagSequence::new(64, CAPTURE, TMS_LOW, TDI_ONES)?,
            JtagSequence::new(64, CAPTURE, TMS_LOW, TDI_ONES)?,
        ];
        let mut num_devices: usize = 0;
        'outer: for _ in 0..2 {
            let tdo = self.send_jtag_sequences(JtagSequenceRequest::new(sequences.clone())?)?;
            for bits in tdo {
                if bits == 0 {
                    num_devices += 8;
                } else {
                    num_devices += bits.trailing_zeros() as usize;
                    break 'outer;
                }
            }
        }

        Ok(num_devices)
    }

    fn send_jtag_sequences(
        &mut self,
        request: JtagSequenceRequest,
    ) -> Result<Vec<u8>, CmsisDapError> {
        commands::send_command::<JtagSequenceRequest>(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                JtagSequenceResponse(Status::DAPOk, tdo) => Ok(tdo),
                JtagSequenceResponse(Status::DAPError, tdo) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn send_swj_sequences(&mut self, request: SequenceRequest) -> Result<(), CmsisDapError> {
        commands::send_command::<SequenceRequest>(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                SequenceResponse(Status::DAPOk) => Ok(()),
                SequenceResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    /// Immediately send whatever is in our batch if it is not empty.
    ///
    /// If the last transfer was a read, result is Some with the read value.
    /// Otherwise, the result is None.
    ///
    /// This will ensure any pending writes are processed and errors from them
    /// raised if necessary.
    fn process_batch(&mut self) -> Result<Option<u32>, ArmError> {
        if self.batch.is_empty() {
            return Ok(None);
        }

        let mut batch = std::mem::take(&mut self.batch);

        tracing::debug!("{} items in batch", batch.len());

        for retry in (0..5).rev() {
            tracing::debug!("Attempting batch of {} items", batch.len());

            let transfers: Vec<InnerTransferRequest> = batch
                .iter()
                .map(|command| match *command {
                    BatchCommand::Read(port, addr) => {
                        InnerTransferRequest::new(port, RW::R, addr as u8, None)
                    }
                    BatchCommand::Write(port, addr, data) => {
                        InnerTransferRequest::new(port, RW::W, addr as u8, Some(data))
                    }
                })
                .collect();

            let response = commands::send_command::<TransferRequest>(
                &mut self.device,
                TransferRequest::new(&transfers),
            )
            .map_err(CmsisDapError::from)
            .map_err(DebugProbeError::from)?;

            let count = response.transfer_count as usize;

            tracing::debug!("{:?} of batch of {} items suceeded", count, batch.len());

            if response.last_transfer_response.protocol_error {
                return Err(DapError::SwdProtocol.into());
            } else {
                match response.last_transfer_response.ack {
                    Ack::Ok => {
                        tracing::trace!("Transfer status: ACK");
                        return Ok(response.transfers[response.transfers.len() - 1].data);
                    }
                    Ack::NoAck => {
                        tracing::trace!("Transfer status: NACK");
                        // TODO: Try a reset?
                        return Err(DapError::NoAcknowledge.into());
                    }
                    Ack::Fault => {
                        tracing::trace!("Transfer status: FAULT");

                        // Check the reason for the fault.
                        let response = RawDapAccess::raw_read_register(
                            self,
                            PortType::DebugPort,
                            Ctrl::ADDRESS,
                        )?;
                        let ctrl = Ctrl::try_from(response)?;
                        tracing::trace!("Ctrl/Stat register value is: {:?}", ctrl);

                        if ctrl.sticky_err() {
                            let mut abort = Abort(0);

                            // Clear sticky error flags.
                            abort.set_stkerrclr(ctrl.sticky_err());

                            RawDapAccess::raw_write_register(
                                self,
                                PortType::DebugPort,
                                Abort::ADDRESS,
                                abort.into(),
                            )?;
                        }

                        tracing::trace!("draining {:?} and retries left {:?}", count, retry);
                        batch.drain(0..count);
                        continue;
                    }
                    Ack::Wait => {
                        tracing::trace!("wait",);

                        return Err(DapError::WaitResponse.into());
                    }
                }
            }
        }

        Err(DapError::FaultResponse.into())
    }

    /// Add a BatchCommand to our current batch.
    ///
    /// If the BatchCommand is a Read, this will immediately process the batch
    /// and return the read value. If the BatchCommand is a write, the write is
    /// executed immediately if the batch is full, otherwise it is queued for
    /// later execution.
    fn batch_add(&mut self, command: BatchCommand) -> Result<Option<u32>, ArmError> {
        tracing::debug!("Adding command to batch: {}", command);

        self.batch.push(command);

        // We always immediately process any reads, which means there will never
        // be more than one read in a batch. We also process whenever the batch
        // is as long as can fit in one packet.
        let max_writes = (self.packet_size as usize - 3) / (1 + 4);
        match command {
            BatchCommand::Read(_, _) => self.process_batch(),
            _ if self.batch.len() == max_writes => self.process_batch(),
            _ => Ok(None),
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
            swo::TransportResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse.into()),
        }
    }

    /// Set SWO port to specified mode.
    ///
    /// Check the probe capabilities to determine which modes are available.
    fn set_swo_mode(&mut self, mode: swo::ModeRequest) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, mode)?;
        match response {
            swo::ModeResponse(Status::DAPOk) => Ok(()),
            swo::ModeResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse.into()),
        }
    }

    /// Set SWO port to specified baud rate.
    ///
    /// Returns `SwoBaudrateNotConfigured` if the probe returns 0,
    /// indicating the requested baud rate was not configured,
    /// and returns the configured baud rate on success (which
    /// may differ from the requested baud rate).
    fn set_swo_baudrate(&mut self, baud: swo::BaudrateRequest) -> Result<u32, DebugProbeError> {
        let response = commands::send_command(&mut self.device, baud)?;
        tracing::debug!("Requested baud {}, got {}", baud.0, response);
        if response == 0 {
            Err(CmsisDapError::SwoBaudrateNotConfigured.into())
        } else {
            Ok(response)
        }
    }

    /// Start SWO trace data capture.
    fn start_swo_capture(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, swo::ControlRequest::Start)?;
        match response {
            swo::ControlResponse(Status::DAPOk) => Ok(()),
            swo::ControlResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse.into()),
        }
    }

    /// Stop SWO trace data capture.
    fn stop_swo_capture(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, swo::ControlRequest::Stop)?;
        match response {
            swo::ControlResponse(Status::DAPOk) => Ok(()),
            swo::ControlResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse.into()),
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
                let n = usize::min(swo_buffer_size, self.packet_size as usize) as u16;

                let response: swo::DataResponse =
                    commands::send_command(&mut self.device, swo::DataRequest { max_count: n })?;
                if response.status.error {
                    Err(CmsisDapError::SwoTraceStreamError.into())
                } else {
                    Ok(response.data)
                }
            }
            None => Ok(Vec::new()),
        }
    }

    fn connect_if_needed(&mut self) -> Result<(), DebugProbeError> {
        if self.connected {
            return Ok(());
        }

        let protocol = if let Some(protocol) = self.protocol {
            match protocol {
                WireProtocol::Swd => ConnectRequest::Swd,
                WireProtocol::Jtag => ConnectRequest::Jtag,
            }
        } else {
            ConnectRequest::DefaultPort
        };

        let used_protocol = commands::send_command(&mut self.device, protocol)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                ConnectResponse::SuccessfulInitForSWD => Ok(WireProtocol::Swd),
                ConnectResponse::SuccessfulInitForJTAG => Ok(WireProtocol::Jtag),
                ConnectResponse::InitFailed => Err(CmsisDapError::ErrorResponse),
            })?;

        // Store the actually used protocol, to handle cases where the default protocol is used.
        tracing::info!("Using protocol {}", used_protocol);
        self.protocol = Some(used_protocol);
        self.connected = true;

        Ok(())
    }
}

impl DebugProbe for CmsisDap {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        Ok(Box::new(Self::new_from_device(
            tools::open_device_from_selector(selector)?,
        )?))
    }

    fn get_name(&self) -> &str {
        "CMSIS-DAP"
    }

    /// Get the currently set maximum speed.
    ///
    /// CMSIS-DAP offers no possibility to get the actual speed used.
    fn speed_khz(&self) -> u32 {
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
    #[tracing::instrument(skip(self))]
    fn attach(&mut self) -> Result<(), DebugProbeError> {
        tracing::debug!("Attaching to target system (clock = {}kHz)", self.speed_khz);

        // Run connect sequence (may already be done earlier via swj operations)
        self.connect_if_needed()?;

        // Set speed after connecting as it can be reset during protocol selection
        self.set_speed(self.speed_khz)?;

        self.transfer_configure(ConfigureRequest {
            idle_cycles: 0,
            wait_retry: 0xffff,
            match_retry: 0,
        })?;

        match self.active_protocol() {
            Some(WireProtocol::Jtag) => {}
            Some(WireProtocol::Swd) => {
                self.configure_swd(swd::configure::ConfigureRequest {})?;
            }
            None => todo!(),
        }

        // Tell the probe we are connected so it can turn on an LED.
        let _: Result<HostStatusResponse, _> =
            commands::send_command(&mut self.device, HostStatusRequest::connected(true));

        Ok(())
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), crate::Error> {
        self.process_batch()?;

        if self.swo_active {
            self.disable_swo()?;
        }

        let response = commands::send_command(&mut self.device, DisconnectRequest {})
            .map_err(|e| DebugProbeError::ProbeSpecific(Box::new(e)))?;

        // Tell probe we are disconnected so it can turn off its LED.
        let _: Result<HostStatusResponse, _> =
            commands::send_command(&mut self.device, HostStatusRequest::connected(false));

        self.connected = false;

        match response {
            DisconnectResponse(Status::DAPOk) => Ok(()),
            DisconnectResponse(Status::DAPError) => {
                Err(crate::Error::Probe(CmsisDapError::ErrorResponse.into()))
            }
        }
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        match protocol {
            WireProtocol::Jtag => {
                // tracing::warn!(
                //     "Support for JTAG protocol is not yet implemented for CMSIS-DAP based probes."
                // );
                // Err(DebugProbeError::UnsupportedProtocol(WireProtocol::Jtag))
                self.protocol = Some(WireProtocol::Jtag);
                Ok(())
            }
            WireProtocol::Swd => {
                self.protocol = Some(WireProtocol::Swd);
                Ok(())
            }
        }
    }

    fn active_protocol(&self) -> Option<WireProtocol> {
        self.protocol
    }

    /// Asserts the nRESET pin.
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        commands::send_command(&mut self.device, ResetRequest).map(|v: ResetResponse| {
            tracing::info!("Target reset response: {:?}", v);
        })?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        let request = SWJPinsRequestBuilder::new().nreset(false).build();

        commands::send_command(&mut self.device, request).map(|v: SWJPinsResponse| {
            tracing::info!("Pin response: {:?}", v);
        })?;
        Ok(())
    }

    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        let request = SWJPinsRequestBuilder::new().nreset(true).build();

        commands::send_command(&mut self.device, request).map(|v: SWJPinsResponse| {
            tracing::info!("Pin response: {:?}", v);
        })?;
        Ok(())
    }

    fn get_swo_interface(&self) -> Option<&dyn SwoAccess> {
        Some(self as _)
    }

    fn get_swo_interface_mut(&mut self) -> Option<&mut dyn SwoAccess> {
        Some(self as _)
    }

    fn try_get_arm_interface<'probe>(
        self: Box<Self>,
    ) -> Result<Box<dyn UninitializedArmProbe + 'probe>, (Box<dyn DebugProbe>, DebugProbeError)>
    {
        Ok(Box::new(ArmCommunicationInterface::new(self, false)))
    }

    fn has_arm_interface(&self) -> bool {
        true
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn try_as_dap_probe(&mut self) -> Option<&mut dyn DapProbe> {
        Some(self)
    }
}

impl RawDapAccess for CmsisDap {
    fn select_dp(&mut self, dp: DpAddress) -> Result<(), ArmError> {
        match dp {
            DpAddress::Default => Ok(()), // nop
            DpAddress::Multidrop(targetsel) => {
                for _i in 0..5 {
                    // Flush just in case there were writes queued from before.
                    self.process_batch()?;

                    let request = SequenceRequest::new(
                        &[
                            0xff, 0x92, 0xf3, 0x09, 0x62, 0x95, 0x2d, 0x85, 0x86, 0xe9, 0xaf, 0xdd,
                            0xe3, 0xa2, 0x0e, 0xbc, 0x19, 0xa0, 0xf1, 0xff, 0xff, 0xff, 0xff, 0xff,
                            0xff, 0xff, 0xff, 0x00,
                        ],
                        28 * 8,
                    )
                    .map_err(DebugProbeError::from)?;

                    // dormant-to-swd + line reset
                    self.send_swj_sequences(request)
                        .map_err(DebugProbeError::from)?;

                    // TARGETSEL write.
                    // The TARGETSEL write is not ACKed by design. We can't use a normal register write
                    // because many probes don't even send the data phase when NAK.
                    let parity = targetsel.count_ones() % 2;
                    let data = &((parity as u64) << 45 | (targetsel as u64) << 13 | 0x1f99)
                        .to_le_bytes()[..6];

                    let request =
                        SequenceRequest::new(data, 6 * 8).map_err(DebugProbeError::from)?;

                    self.send_swj_sequences(request)
                        .map_err(DebugProbeError::from)?;

                    // "A write to the TARGETSEL register must always be followed by a read of the DPIDR register or a line reset. If the
                    // response to the DPIDR read is incorrect, or there is no response, the host must start the sequence again."
                    match self.raw_read_register(PortType::DebugPort, 0) {
                        Ok(res) => {
                            tracing::debug!("DPIDR read {:08x}", res);
                            return Ok(());
                        }
                        Err(e) => {
                            tracing::debug!("DPIDR read failed, retrying. Error: {:?}", e);
                        }
                    }
                }
                tracing::warn!("Giving up on TARGETSEL, too many retries.");
                Err(DapError::NoAcknowledge.into())
            }
        }
    }

    /// Reads the DAP register on the specified port and address.
    fn raw_read_register(&mut self, port: PortType, addr: u8) -> Result<u32, ArmError> {
        let res = self.batch_add(BatchCommand::Read(port, addr as u16))?;

        // NOTE(unwrap): batch_add will always return Some if the last command is a read
        // and running the batch was successful.
        Ok(res.unwrap())
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn raw_write_register(&mut self, port: PortType, addr: u8, value: u32) -> Result<(), ArmError> {
        self.batch_add(BatchCommand::Write(port, addr as u16, value))
            .map(|_| ())
    }

    fn raw_write_block(
        &mut self,
        port: PortType,
        register_address: u8,
        values: &[u32],
    ) -> Result<(), ArmError> {
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

        let max_packet_size_words = (self.packet_size - 6) / 4;

        let data_chunk_len = max_packet_size_words as usize;

        for (i, chunk) in values.chunks(data_chunk_len).enumerate() {
            let request =
                TransferBlockRequest::write_request(register_address, port, Vec::from(chunk));

            tracing::debug!("Transfer block: chunk={}, len={} bytes", i, chunk.len() * 4);

            let resp: TransferBlockResponse =
                commands::send_command(&mut self.device, request).map_err(DebugProbeError::from)?;

            if resp.transfer_response != 1 {
                return Err(DebugProbeError::from(CmsisDapError::ErrorResponse).into());
            }
        }

        Ok(())
    }

    fn raw_read_block(
        &mut self,
        port: PortType,
        register_address: u8,
        values: &mut [u32],
    ) -> Result<(), ArmError> {
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

        let max_packet_size_words = (self.packet_size - 6) / 4;

        let data_chunk_len = max_packet_size_words as usize;

        for (i, chunk) in values.chunks_mut(data_chunk_len).enumerate() {
            let request =
                TransferBlockRequest::read_request(register_address, port, chunk.len() as u16);

            tracing::debug!("Transfer block: chunk={}, len={} bytes", i, chunk.len() * 4);

            let resp: TransferBlockResponse =
                commands::send_command(&mut self.device, request).map_err(DebugProbeError::from)?;

            if resp.transfer_response != 1 {
                return Err(DebugProbeError::from(CmsisDapError::ErrorResponse).into());
            }

            chunk.clone_from_slice(&resp.transfer_data[..]);
        }

        Ok(())
    }

    fn raw_flush(&mut self) -> Result<(), ArmError> {
        self.process_batch()?;
        Ok(())
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }

    fn jtag_enumerate(&mut self) -> Result<u16, DebugProbeError> {
        let scan = self.jtag_scan();

        let res = self.jtag_enumerate()?;
        Ok(res)
    }

    fn jtag_sequence(&mut self, cycles: u8, tms: bool, tdi: u64) -> Result<(), DebugProbeError> {
        self.connect_if_needed()?;

        let tdi_bytes = tdi.to_le_bytes();
        let sequence = JtagSequence::new(cycles, false, tms, tdi_bytes)?;
        let sequences = vec![sequence];

        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    fn swj_sequence(&mut self, bit_len: u8, bits: u64) -> Result<(), DebugProbeError> {
        self.connect_if_needed()?;

        let data = bits.to_le_bytes();

        self.send_swj_sequences(SequenceRequest::new(&data, bit_len)?)?;

        Ok(())
    }

    fn swj_pins(
        &mut self,
        pin_out: u32,
        pin_select: u32,
        pin_wait: u32,
    ) -> Result<u32, DebugProbeError> {
        self.connect_if_needed()?;

        let request = SWJPinsRequest::from_raw_values(pin_out as u8, pin_select as u8, pin_wait);

        let Pins(response) = commands::send_command(&mut self.device, request)?;

        Ok(response as u32)
    }
}

impl DapProbe for CmsisDap {}

impl SwoAccess for CmsisDap {
    fn enable_swo(&mut self, config: &SwoConfig) -> Result<(), ArmError> {
        let caps = self.capabilities;

        // Check requested mode is available in probe capabilities
        match config.mode() {
            SwoMode::Uart if !caps.swo_uart_implemented => {
                return Err(DebugProbeError::ProbeSpecific(
                    CmsisDapError::SwoModeNotAvailable.into(),
                )
                .into())
            }
            SwoMode::Manchester if !caps.swo_manchester_implemented => {
                return Err(DebugProbeError::ProbeSpecific(
                    CmsisDapError::SwoModeNotAvailable.into(),
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
        if caps.swo_streaming_trace_implemented && self.device.swo_streaming_supported() {
            tracing::debug!("Starting SWO capture with streaming transport");
            self.set_swo_transport(swo::TransportRequest::WinUsbEndpoint)?;
            self.swo_streaming = true;
        } else {
            tracing::debug!("Starting SWO capture with polled transport");
            self.set_swo_transport(swo::TransportRequest::DataCommand)?;
            self.swo_streaming = false;
        }

        // Set mode. We've already checked that the requested mode is listed as supported.
        match config.mode() {
            SwoMode::Uart => self.set_swo_mode(swo::ModeRequest::Uart)?,
            SwoMode::Manchester => self.set_swo_mode(swo::ModeRequest::Manchester)?,
        }

        // Set baud rate.
        let baud = self.set_swo_baudrate(swo::BaudrateRequest(config.baud()))?;
        if baud != config.baud() {
            tracing::warn!(
                "Target SWO baud rate not met: requested {}, got {}",
                config.baud(),
                baud
            );
        }

        self.start_swo_capture()?;

        self.swo_active = true;
        Ok(())
    }

    fn disable_swo(&mut self) -> Result<(), ArmError> {
        tracing::debug!("Stopping SWO capture");
        self.stop_swo_capture()?;
        self.swo_active = false;
        Ok(())
    }

    fn read_swo_timeout(&mut self, timeout: Duration) -> Result<Vec<u8>, ArmError> {
        if self.swo_active {
            if self.swo_streaming {
                let buffer = self
                    .device
                    .read_swo_stream(timeout)
                    .map_err(DebugProbeError::from)?;
                tracing::trace!("SWO streaming buffer: {:?}", buffer);
                Ok(buffer)
            } else {
                let data = self.get_swo_data()?;
                tracing::trace!("SWO polled data: {:?}", data);
                Ok(data)
            }
        } else {
            Ok(Vec::new())
        }
    }

    fn swo_poll_interval_hint(&mut self, config: &SwoConfig) -> Option<std::time::Duration> {
        let caps = self.capabilities;
        if caps.swo_streaming_trace_implemented && self.device.swo_streaming_supported() {
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

impl Drop for CmsisDap {
    fn drop(&mut self) {
        tracing::debug!("Detaching from CMSIS-DAP probe");
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
