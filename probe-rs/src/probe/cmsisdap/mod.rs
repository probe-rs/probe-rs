//! CMSIS-DAP probe implementation.
mod commands;
mod tools;

use crate::{
    architecture::arm::{
        communication_interface::{DapProbe, UninitializedArmProbe},
        dp::{Abort, Ctrl},
        swo::poll_interval_from_buf_size,
        ArmCommunicationInterface, ArmError, DapError, Pins, PortType, RawDapAccess, Register,
        SwoAccess, SwoConfig, SwoMode,
    },
    probe::{
        cmsisdap::commands::{
            general::info::{CapabilitiesCommand, PacketCountCommand, SWOTraceBufferSizeCommand},
            CmsisDapError,
        },
        BatchCommand, DebugProbe, DebugProbeError, DebugProbeInfo, DebugProbeSelector,
        JtagChainItem, ProbeFactory, WireProtocol,
    },
    CoreStatus,
};

use commands::{
    general::{
        connect::{ConnectRequest, ConnectResponse},
        disconnect::{DisconnectRequest, DisconnectResponse},
        host_status::{HostStatusRequest, HostStatusResponse},
        info::Capabilities,
        reset::{ResetRequest, ResetResponse},
    },
    jtag::{
        configure::{
            ConfigureRequest as JtagConfigureRequest, ConfigureResponse as JtagConfigureResponse,
        },
        sequence::{
            Sequence as JtagSequence, SequenceRequest as JtagSequenceRequest,
            SequenceResponse as JtagSequenceResponse,
        },
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
use probe_rs_target::ScanChainElement;

use std::{fmt::Write, time::Duration};

use bitvec::prelude::*;

use super::common::{extract_idcodes, extract_ir_lengths, ScanChainError};

/// A factory for creating [`CmsisDap`] probes.
#[derive(Debug)]
pub struct CmsisDapFactory;

impl std::fmt::Display for CmsisDapFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CMSIS-DAP")
    }
}

impl ProbeFactory for CmsisDapFactory {
    fn open(&self, selector: &DebugProbeSelector) -> Result<Box<dyn DebugProbe>, DebugProbeError> {
        Ok(Box::new(CmsisDap::new_from_device(
            tools::open_device_from_selector(selector)?,
        )?))
    }

    fn list_probes(&self) -> Vec<DebugProbeInfo> {
        tools::list_cmsisdap_devices()
    }
}

/// A CMSIS-DAP probe.
pub struct CmsisDap {
    device: CmsisDapDevice,
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
    scan_chain: Option<Vec<ScanChainElement>>,

    batch: Vec<BatchCommand>,
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
    fn new_from_device(mut device: CmsisDapDevice) -> Result<Self, DebugProbeError> {
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
            scan_chain: None,
            batch: Vec::new(),
        })
    }

    /// Set maximum JTAG/SWD clock frequency to use, in Hz.
    ///
    /// The actual clock frequency used by the device might be lower.
    fn set_swj_clock(&mut self, clock_hz: u32) -> Result<(), CmsisDapError> {
        commands::send_command(&mut self.device, SWJClockRequest(clock_hz))
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                SWJClockResponse(Status::DAPOk) => Ok(()),
                SWJClockResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn transfer_configure(&mut self, request: ConfigureRequest) -> Result<(), CmsisDapError> {
        commands::send_command(&mut self.device, request)
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
        commands::send_command(&mut self.device, request)
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
        let sequence = JtagSequence::no_capture(true, &bitvec![u8, Lsb0; 0; 6])?;
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
        let sequence = JtagSequence::no_capture(false, &bitvec![u8, Lsb0; 0; 1])?;
        let sequences = vec![sequence];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    /// Scan JTAG chain, detecting TAPs and their IDCODEs and IR lengths.
    ///
    /// If IR lengths for each TAP are known, provide them in `ir_lengths`.
    ///
    /// Returns a new JTAG chain.
    fn jtag_scan(
        &mut self,
        ir_lengths: Option<&[usize]>,
    ) -> Result<Vec<JtagChainItem>, CmsisDapError> {
        let (ir, dr) = self.jtag_reset_scan()?;
        let idcodes = extract_idcodes(&dr)?;
        let ir_lens = extract_ir_lengths(&ir, idcodes.len(), ir_lengths)?;

        Ok(idcodes
            .into_iter()
            .zip(ir_lens)
            .map(|(idcode, irlen)| JtagChainItem { irlen, idcode })
            .collect())
    }

    /// Capture the power-up scan chain values, including all IDCODEs.
    ///
    /// Returns the IR and DR results as (IR, DR).
    fn jtag_reset_scan(&mut self) -> Result<(BitVec<u8>, BitVec<u8>), CmsisDapError> {
        let dr = self.jtag_scan_dr()?;
        let ir = self.jtag_scan_ir()?;

        // Return to Run-Test/Idle, so the probe is ready for DAP_Transfer commands again.
        self.jtag_ensure_run_test_idle()?;

        Ok((ir, dr))
    }

    /// Detect the IR chain length and return its current contents.
    ///
    /// Replaces the current contents with all 1s (BYPASS) and enters
    /// the Run-Test/Idle state.
    fn jtag_scan_ir(&mut self) -> Result<BitVec<u8>, CmsisDapError> {
        self.jtag_ensure_shift_ir()?;
        let data = self.jtag_scan_inner("IR")?;
        Ok(data)
    }

    /// Detect the DR chain length and return its contents.
    ///
    /// Replaces the current contents with all 1s and enters
    /// the Run-Test/Idle state.
    fn jtag_scan_dr(&mut self) -> Result<BitVec<u8>, CmsisDapError> {
        self.jtag_ensure_shift_dr()?;
        let data = self.jtag_scan_inner("DR")?;
        Ok(data)
    }

    /// Detect current chain length and return its contents.
    /// Must already be in either Shift-IR or Shift-DR state.
    fn jtag_scan_inner(&mut self, name: &'static str) -> Result<BitVec<u8>, CmsisDapError> {
        // Max scan chain length (in bits) to attempt to detect.
        const MAX_LENGTH: usize = 128;
        // How many bytes to write out / read in per request.
        const BYTES_PER_REQUEST: usize = 16;
        // How many requests are needed to read/write at least MAX_LENGTH bits.
        const REQUESTS: usize =
            (MAX_LENGTH + (BYTES_PER_REQUEST * 8 - 1)) / (BYTES_PER_REQUEST * 8);

        // Completely fill xR with 0s, capture result.
        let mut tdo_bytes: Vec<u8> = Vec::with_capacity(REQUESTS * BYTES_PER_REQUEST);
        for _ in 0..REQUESTS {
            let sequences = vec![
                JtagSequence::capture(false, &bitvec![u8, Lsb0; 0; 64])?,
                JtagSequence::capture(false, &bitvec![u8, Lsb0; 0; 64])?,
            ];

            tdo_bytes.extend(
                self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?
                    .iter(),
            );
        }
        let d0 = tdo_bytes.view_bits::<Lsb0>();

        // Completely fill xR with 1s, capture result.
        let mut tdo_bytes: Vec<u8> = Vec::with_capacity(REQUESTS * BYTES_PER_REQUEST);
        for _ in 0..REQUESTS {
            let sequences = vec![
                JtagSequence::capture(false, &bitvec![u8, Lsb0; 1; 64])?,
                JtagSequence::capture(false, &bitvec![u8, Lsb0; 1; 64])?,
            ];

            tdo_bytes.extend(
                self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?
                    .iter(),
            );
        }
        let d1 = tdo_bytes.view_bits::<Lsb0>();

        // Find first 1 in d1, which indicates length of register.
        let n = match d1.first_one() {
            Some(n) => {
                tracing::info!("JTAG {name} scan chain detected as {n} bits long");
                n
            }
            None => {
                tracing::error!(
                    "JTAG {name} scan chain either broken or too long: did not detect 1"
                );
                return Err(CmsisDapError::ErrorResponse);
            }
        };

        // Check at least one register is detected in the scan chain.
        if n == 0 {
            tracing::error!("JTAG {name} scan chain is empty");
            return Err(CmsisDapError::ErrorResponse);
        }

        // Check d0[n..] are all 0.
        if d0[n..].any() {
            tracing::error!("JTAG {name} scan chain either broken or too long: did not detect 0");
            return Err(CmsisDapError::ErrorResponse);
        }

        // Extract d0[..n] as the initial scan chain contents.
        let data = d0[..n].to_bitvec();

        Ok(data)
    }

    fn jtag_ensure_shift_dr(&mut self) -> Result<(), CmsisDapError> {
        // Transition to Test-Logic-Reset.
        self.jtag_ensure_test_logic_reset()?;

        // Transition to Shift-DR
        let sequences = vec![
            JtagSequence::no_capture(false, &bitvec![u8, Lsb0; 0; 1])?,
            JtagSequence::no_capture(true, &bitvec![u8, Lsb0; 0; 1])?,
            JtagSequence::no_capture(false, &bitvec![u8, Lsb0; 0; 2])?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    fn jtag_ensure_shift_ir(&mut self) -> Result<(), CmsisDapError> {
        // Transition to Test-Logic-Reset.
        self.jtag_ensure_test_logic_reset()?;

        // Transition to Shift-IR
        let sequences = vec![
            JtagSequence::no_capture(false, &bitvec![u8, Lsb0; 0; 1])?,
            JtagSequence::no_capture(true, &bitvec![u8, Lsb0; 0; 2])?,
            JtagSequence::no_capture(false, &bitvec![u8, Lsb0; 0; 2])?,
        ];
        self.send_jtag_sequences(JtagSequenceRequest::new(sequences)?)?;

        Ok(())
    }

    fn send_jtag_configure(&mut self, request: JtagConfigureRequest) -> Result<(), CmsisDapError> {
        commands::send_command(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                JtagConfigureResponse(Status::DAPOk) => Ok(()),
                JtagConfigureResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn send_jtag_sequences(
        &mut self,
        request: JtagSequenceRequest,
    ) -> Result<Vec<u8>, CmsisDapError> {
        commands::send_command(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                JtagSequenceResponse(Status::DAPOk, tdo) => Ok(tdo),
                JtagSequenceResponse(Status::DAPError, _) => Err(CmsisDapError::ErrorResponse),
            })
    }

    fn send_swj_sequences(&mut self, request: SequenceRequest) -> Result<(), CmsisDapError> {
        // Ensure all pending commands are processed.
        //self.process_batch()?;

        commands::send_command(&mut self.device, request)
            .map_err(CmsisDapError::from)
            .and_then(|v| match v {
                SequenceResponse(Status::DAPOk) => Ok(()),
                SequenceResponse(Status::DAPError) => Err(CmsisDapError::ErrorResponse),
            })
    }

    /// Read the CTRL register from the currently selected debug port.
    ///
    /// According to the ARM specification, this *should* never fail.
    /// In practice, it can unfortunately happen.
    ///
    /// To avoid an endless recursion in this cases, this function is provided
    /// as an alternative to [`Self::process_batch()`]. This function will return any errors,
    /// and not retry any transfers.
    fn read_ctrl_register(&mut self) -> Result<Ctrl, ArmError> {
        let response = commands::send_command(
            &mut self.device,
            TransferRequest::new(&[InnerTransferRequest::new(
                PortType::DebugPort,
                RW::R,
                Ctrl::ADDRESS,
                None,
            )]),
        )
        .map_err(CmsisDapError::from)
        .map_err(DebugProbeError::from)?;

        // We can assume that the single transfer is always executed,
        // no need to check here.

        if response.last_transfer_response.protocol_error {
            // TODO: What does this protocol error mean exactly?
            //       Should be verified in CMSIS-DAP spec
            Err(DapError::SwdProtocol.into())
        } else {
            if response.last_transfer_response.ack != Ack::Ok {
                tracing::debug!(
                    "Error reading debug port CTRL register: {:?}. This should never fail!",
                    response.last_transfer_response.ack
                );
            }

            match response.last_transfer_response.ack {
                Ack::Ok => {
                    Ok(Ctrl(response.transfers[0].data.expect(
                        "CMSIS-DAP probe should always return data for a read.",
                    )))
                }
                Ack::Wait => Err(DapError::WaitResponse.into()),
                Ack::Fault => Err(DapError::FaultResponse.into()),
                Ack::NoAck => Err(DapError::NoAcknowledge.into()),
            }
        }
    }

    /// Immediately send whatever is in our batch if it is not empty.
    ///
    /// If the last transfer was a read, result is Some with the read value.
    /// Otherwise, the result is None.
    ///
    /// This will ensure any pending writes are processed and errors from them
    /// raised if necessary.
    #[tracing::instrument(skip(self))]
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

            let response =
                commands::send_command(&mut self.device, TransferRequest::new(&transfers))
                    .map_err(CmsisDapError::from)
                    .map_err(DebugProbeError::from)?;

            let count = response.transfer_count as usize;

            tracing::debug!("{:?} of batch of {} items executed", count, batch.len());

            if response.last_transfer_response.protocol_error {
                if count > 0 {
                    tracing::debug!("Protocol error in response to command {}", batch[count - 1]);
                }

                return Err(DapError::SwdProtocol.into());
            }

            match response.last_transfer_response.ack {
                Ack::Ok => {
                    tracing::trace!("Transfer status: ACK");
                    return Ok(response.transfers[response.transfers.len() - 1].data);
                }
                Ack::NoAck => {
                    tracing::trace!(
                        "Transfer status for batch item {}/{}: NACK",
                        count,
                        batch.len()
                    );
                    // TODO: Try a reset?
                    return Err(DapError::NoAcknowledge.into());
                }
                Ack::Fault => {
                    tracing::trace!(
                        "Transfer status for batch item {}/{}: FAULT",
                        count,
                        batch.len()
                    );

                    // To avoid a potential endless recursion,
                    // call a separate function to read the ctrl register,
                    // which doesn't use the batch API.
                    let ctrl = self.read_ctrl_register()?;

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

                    let mut abort = Abort(0);
                    abort.set_dapabort(true);

                    RawDapAccess::raw_write_register(
                        self,
                        PortType::DebugPort,
                        Abort::ADDRESS,
                        abort.into(),
                    )?;

                    return Err(DapError::WaitResponse.into());
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

    fn set_scan_chain(&mut self, scan_chain: Vec<ScanChainElement>) -> Result<(), DebugProbeError> {
        tracing::info!("Setting scan chain to {:?}", scan_chain);
        self.scan_chain = Some(scan_chain);
        Ok(())
    }

    /// Returns the JTAG scan chain
    fn scan_chain(&self) -> Result<&[ScanChainElement], DebugProbeError> {
        match self.active_protocol() {
            Some(WireProtocol::Jtag) => {
                if let Some(ref chain) = self.scan_chain {
                    Ok(chain.as_slice())
                } else {
                    Ok(&[])
                }
            }
            _ => Err(DebugProbeError::InterfaceNotAvailable {
                interface_name: "JTAG",
            }),
        }
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

        if self.active_protocol() == Some(WireProtocol::Jtag) {
            // no-op: we configure JTAG in debug_port_setup,
            // because that is where we execute the SWJ-DP Switch Sequence
            // to ensure the debug port is ready for JTAG signals,
            // at which point we can interrogate the scan chain
            // and configure the probe with the given IR lengths.
        } else {
            self.configure_swd(swd::configure::ConfigureRequest {})?;
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
            WireProtocol::Jtag if self.capabilities._jtag_implemented => {
                self.protocol = Some(WireProtocol::Jtag);
                Ok(())
            }
            WireProtocol::Swd if self.capabilities._swd_implemented => {
                self.protocol = Some(WireProtocol::Swd);
                Ok(())
            }
            _ => Err(DebugProbeError::UnsupportedProtocol(protocol)),
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
    fn core_status_notification(&mut self, status: CoreStatus) -> Result<(), DebugProbeError> {
        let running = status.is_running();
        commands::send_command(&mut self.device, HostStatusRequest::running(running))?;
        Ok(())
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

    fn configure_jtag(&mut self) -> Result<(), DebugProbeError> {
        let chain = self.jtag_scan(
            self.scan_chain
                .as_ref()
                .map(|chain| {
                    chain
                        .iter()
                        .filter_map(|s| s.ir_len)
                        .map(|s| s as usize)
                        .collect::<Vec<usize>>()
                })
                .as_deref(),
        )?;
        let ir_lengths = chain.iter().map(|item| item.irlen as u8).collect();

        tracing::info!("Configuring JTAG with ir lengths: {:?}", ir_lengths);
        self.send_jtag_configure(JtagConfigureRequest::new(ir_lengths)?)?;

        Ok(())
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

        if tracing::enabled!(tracing::Level::TRACE) {
            let mut seq = String::new();

            let _ = write!(&mut seq, "swj sequence:");

            for i in 0..bit_len {
                let bit = (bits >> i) & 1;

                if bit == 1 {
                    let _ = write!(&mut seq, "1");
                } else {
                    let _ = write!(&mut seq, "0");
                }
            }
            tracing::trace!("{}", seq);
        }

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

impl From<ScanChainError> for CmsisDapError {
    fn from(error: ScanChainError) -> Self {
        match error {
            ScanChainError::InvalidIdCode => CmsisDapError::InvalidIdCode,
            ScanChainError::InvalidIR => CmsisDapError::InvalidIR,
        }
    }
}
