pub mod commands;
pub mod tools;

use crate::{Memory, DebugProbeError, DebugProbe, DebugProbeInfo, WireProtocol};
use crate::architecture::arm::dp::{DPRegister, DPAccess, DebugPort};
use crate::architecture::arm::PortType;
use crate::architecture::arm::DAPAccess;
use crate::probe::daplink::commands::Error;
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
use log::{debug, error, info};
use std::cell::RefCell;
use std::rc::Rc;

use std::sync::Mutex;

pub struct DAPLink {
    pub device: Mutex<hidapi::HidDevice>,
    _hw_version: u8,
    _jtag_version: u8,
    _protocol: WireProtocol,

    packet_size: Option<u16>,
    packet_count: Option<u8>,
}

impl DAPLink {
    pub fn new_from_device(device: hidapi::HidDevice) -> Self {
        Self {
            device: Mutex::new(device),
            _hw_version: 0,
            _jtag_version: 0,
            _protocol: WireProtocol::Swd,
            packet_count: None,
            packet_size: None,
        }
    }

    fn set_swj_clock(&mut self, clock: u32) -> Result<(), Error> {
        commands::send_command::<SWJClockRequest, SWJClockResponse>(
            &mut self.device,
            SWJClockRequest(clock),
        )
        .and_then(|v| match v {
            SWJClockResponse(Status::DAPOk) => Ok(()),
            SWJClockResponse(Status::DAPError) => Err(Error::DAP),
        })?;
        Ok(())
    }

    fn transfer_configure(&mut self, request: ConfigureRequest) -> Result<(), Error> {
        commands::send_command::<ConfigureRequest, ConfigureResponse>(&mut self.device, request)
            .and_then(|v| match v {
                ConfigureResponse(Status::DAPOk) => Ok(()),
                ConfigureResponse(Status::DAPError) => Err(Error::DAP),
            })?;
        Ok(())
    }

    fn configure_swd(&mut self, request: swd::configure::ConfigureRequest) -> Result<(), Error> {
        commands::send_command::<swd::configure::ConfigureRequest, swd::configure::ConfigureResponse>(
            &mut self.device,
            request
        )
        .and_then(|v| match v {
            swd::configure::ConfigureResponse(Status::DAPOk) => Ok(()),
            swd::configure::ConfigureResponse(Status::DAPError) => Err(Error::DAP),
        })?;
        Ok(())
    }

    fn send_swj_sequences(&mut self, request: SequenceRequest) -> Result<(), Error> {
        /* 12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence
        12 10 9E E7 -> 12 00 // SWJ Sequence
        12 38 FF FF FF FF FF FF FF -> 12 00 // SWJ Sequence */
        //let sequence_1 = SequenceRequest::new(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);

        commands::send_command::<SequenceRequest, SequenceResponse>(&mut self.device, request)
            .and_then(|v| match v {
                SequenceResponse(Status::DAPOk) => Ok(()),
                SequenceResponse(Status::DAPError) => Err(Error::DAP),
            })?;
        Ok(())
    }
}

impl<P: DebugPort, R: DPRegister<P>> DPAccess<P, R> for DAPLink {
    type Error = DebugProbeError;

    fn read_dp_register(&mut self, _port: &P) -> Result<R, Self::Error> {
        debug!("Reading DP register {}", R::NAME);
        let result = self.read_register(PortType::DebugPort, u16::from(R::ADDRESS))?;

        debug!("Read    DP register {}, value=0x{:08x}", R::NAME, result);

        Ok(result.into())
    }

    fn write_dp_register(&mut self, _port: &P, register: R) -> Result<(), Self::Error> {
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

    /// Enters debug mode.
    fn attach(&mut self, protocol: Option<WireProtocol>) -> Result<WireProtocol, DebugProbeError> {
        // get information about the daplink
        let PacketCount(packet_count) =
            commands::send_command(&mut self.device, Command::PacketCount)?;
        let PacketSize(packet_size) =
            commands::send_command(&mut self.device, Command::PacketSize)?;

        self.packet_count = Some(packet_count);
        self.packet_size = Some(packet_size);

        let clock = 1_000_000;

        info!("Attaching to target system (clock = {})", clock);
        self.set_swj_clock(clock)?;

        let protocol = if let Some(protocol) = protocol {
            match protocol {
                WireProtocol::Swd => ConnectRequest::UseSWD,
                WireProtocol::Jtag => ConnectRequest::UseJTAG,
            }
        } else {
            ConnectRequest::UseDefaultPort
        };

        let result = commands::send_command(&mut self.device, protocol).and_then(|v| match v {
            ConnectResponse::SuccessfulInitForSWD => Ok(WireProtocol::Swd),
            ConnectResponse::SuccessfulInitForJTAG => Ok(WireProtocol::Jtag),
            ConnectResponse::InitFailed => Err(Error::DAP),
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

        use crate::architecture::arm::dp::{Abort, Ctrl, DPv1, DebugPortId, Select, DPIDR};

        // assume a dpv1 port for now

        let port = DPv1 {};

        let dp_id: DPIDR = self.read_dp_register(&port)?;

        let dp_id: DebugPortId = dp_id.into();

        info!("Debug PortType Version:  {:x?}", dp_id.version);
        info!(
            "Debug PortType Designer: {}",
            dp_id.designer.get().unwrap_or("Unknown")
        );

        let mut abort_reg = Abort(0);
        abort_reg.set_orunerrclr(true);
        abort_reg.set_wderrclr(true);
        abort_reg.set_stkerrclr(true);
        abort_reg.set_stkcmpclr(true);

        self.write_dp_register(&port, abort_reg)?; // clear errors

        let mut select_reg = Select(0);
        select_reg.set_dp_bank_sel(0);

        self.write_dp_register(&port, select_reg)?; // select DBPANK 0

        let mut ctrl_reg = Ctrl::default();

        ctrl_reg.set_csyspwrupreq(true);
        ctrl_reg.set_cdbgpwrupreq(true);

        debug!("Requesting debug power");

        self.write_dp_register(&port, ctrl_reg)?; // CSYSPWRUPREQ, CDBGPWRUPREQ

        // TODO: Check return value if power up was ok
        let ctrl_reg: Ctrl = self.read_dp_register(&port)?;

        if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
            error!("Debug power request failed");
            return Err(Error::TargetPowerUpFailed.into());
        }

        info!("Succesfully attached to system and entered debug mode");

        Ok(result)
    }

    /// Leave debug mode.
    fn detach(&mut self) -> Result<(), DebugProbeError> {
        let response = commands::send_command(&mut self.device, DisconnectRequest {})?;

        match response {
            DisconnectResponse(Status::DAPOk) => Ok(()),
            DisconnectResponse(Status::DAPError) => Err(Error::UnexpectedAnswer.into()),
        }
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
                Err(Error::SwdProtocol.into())
            } else {
                match response.transfer_response.ack {
                    Ack::Ok => Ok(response.transfer_data),
                    Ack::NoAck => Err(Error::NoAcknowledge.into()),
                    Ack::Fault => Err(Error::DeviceFault.into()),
                    Ack::Wait => Err(Error::Wait.into()),
                }
            }
        } else {
            Err(Error::UnexpectedAnswer.into())
        }
    }

    /// Writes a value to the DAP register on the specified port and address.
    fn write_register(&mut self, port: PortType, addr: u16, value: u32) -> Result<(), DebugProbeError> {
        let response = commands::send_command::<TransferRequest, TransferResponse>(
            &mut self.device,
            TransferRequest::new(InnerTransferRequest::new(port.into(), RW::W, addr as u8), value),
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
