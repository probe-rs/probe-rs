use crate::error;
use anyhow::Result;
use thiserror::Error;

//use log::debug;
use scroll::{Pread, Pwrite, LE};

use crate::architecture::avr::communication_interface::AvrCommunicationInterface;
use crate::probe::cmsisdap;
use crate::probe::cmsisdap::commands;
use crate::probe::cmsisdap::commands::edbg::{
    avr_cmd::AvrCommand, avr_evt::AvrEventRequest, avr_rsp::AvrRSPRequest,
};
use crate::probe::cmsisdap::commands::CmsisDapDevice;
use crate::DebugProbe;
use crate::DebugProbeError;
use crate::DebugProbeSelector;
use crate::WireProtocol;
use crate::{CoreInformation, CoreStatus, HaltReason};
use enum_primitive_derive::Primitive;
use num_traits::FromPrimitive;

use std::time::Duration;

use std::fmt;

pub mod avr8generic;

mod housekeeping;

pub mod tools;

#[derive(Debug, Error)]
pub enum EdbgError {
    #[error("Debugger returned error code")]
    ErrorCode(avr8generic::FailureCodes),
    #[error("Unexpected response to command")]
    UnexpectedResponse,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<EdbgError> for DebugProbeError {
    fn from(error: EdbgError) -> Self {
        DebugProbeError::ProbeSpecific(Box::new(error))
    }
}

pub struct EDBG {
    pub device: CmsisDapDevice,
    pub speed_khz: u32,
    pub sequence_number: u16,
    pub protocol: Option<AvrWireProtocol>,
}

#[derive(Copy, Clone, PartialEq, Debug, serde::Serialize, serde::Deserialize)]
pub enum AvrWireProtocol {
    Jtag,
    DebugWire,
    Pdi,
    Updi,
}

impl fmt::Display for AvrWireProtocol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AvrWireProtocol::Jtag => write!(f, "JTAG"),
            AvrWireProtocol::DebugWire => write!(f, "DebugWire"),
            AvrWireProtocol::Pdi => write!(f, "PDI"),
            AvrWireProtocol::Updi => write!(f, "UPDI"),
        }
    }
}

impl std::str::FromStr for AvrWireProtocol {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_ascii_lowercase()[..] {
            "jtag" => Ok(AvrWireProtocol::Jtag),
            "DebugWire" => Ok(AvrWireProtocol::DebugWire),
            "pdi" => Ok(AvrWireProtocol::Pdi),
            "updi" => Ok(AvrWireProtocol::Updi),
            _ => Err(format!(
                "'{}' is not a valid avr protocol. Choose from [jtag, DebugWire, pdi, updi].",
                s
            )),
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Jtagice3DiscoveryCommands {
    CmdQuery = 0x00,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq)]
enum Jtagice3DiscoveryResponses {
    RspDiscoveryList = 0x81,
    RspDiscoveryFailed = 0xA0,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum Jtagice3FailureCodes {
    FailureOk = 0x00,
    FailureUsbPrevoiusUnderrun = 0xE0,
    FailureUnknown = 0xFF,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum Jtagice3Discovery {
    DiscoveryCommandHandlers = 0x00,
    DiscoveryToolName = 0x80,
    DiscoverySerialNumber = 0x81,
    DiscoveryMfnDate = 0x82,
}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum Jtagice3DiscoveryFailureCodes {
    DiscoveryFailedNotSupported = 0x10,
}

const EDBG_SOF: u8 = 0x0E;

#[allow(dead_code)]
#[derive(Clone, Copy, Debug, Primitive, PartialEq)]
enum SubProtocols {
    Discovery = 0x00,
    Housekeeping = 0x01,
    AVRISP = 0x11,
    AVR8Generic = 0x12,
    AVR32Generic = 0x13,
    TPI = 0x14,
    EDBGCtrl = 0x20,
}

#[allow(dead_code)]
#[derive(Copy, Clone, Debug)]
enum AddressSize {
    Size24bit = 0x01,
    Size16bit = 0x00,
}

#[derive(Copy, Clone, Debug)]
struct TinyXDeviceData {
    prog_base: u32,
    flash_pages_bytes: u16,
    eeprom_pages_bytes: u8,
    nvmctrl_module_address: u16,
    ocd_module_address: u16,
    //_padding: [u8; 10],
    flash_bytes: u32,
    eeprom_bytes: u16,
    user_sig_bytes_bytes: u16,
    fuse_bytes: u8,
    //padding: [u8; 5]
    eeprom_base: u16,
    user_row_base: u16,
    sigrow_base: u16,
    fuses_base: u16,
    lock_base: u16,
    device_id: u32,
    //prog_base_msb
    //flash_pages_bytes_msb
    address_size: AddressSize,
}

impl TinyXDeviceData {
    pub fn to_device_data(&self) -> Vec<u8> {
        let mut data = vec![0u8; 0x2f];

        data.pwrite_with(self.prog_base as u16, 0, LE).unwrap();
        data.pwrite_with(self.flash_pages_bytes as u8, 2, LE)
            .unwrap();
        data.pwrite_with(self.eeprom_pages_bytes as u8, 3, LE)
            .unwrap();
        data.pwrite_with(self.nvmctrl_module_address as u16, 4, LE)
            .unwrap();
        data.pwrite_with(self.ocd_module_address as u16, 6, LE)
            .unwrap();

        data.pwrite_with(self.flash_bytes as u32, 0x12, LE).unwrap();
        data.pwrite_with(self.eeprom_bytes as u16, 0x16, LE)
            .unwrap();
        data.pwrite_with(self.user_sig_bytes_bytes as u16, 0x18, LE)
            .unwrap();
        data.pwrite_with(self.fuse_bytes as u8, 0x1a, LE).unwrap();

        data.pwrite_with(self.eeprom_base as u16, 0x20, LE).unwrap();
        data.pwrite_with(self.user_row_base as u16, 0x22, LE)
            .unwrap();
        data.pwrite_with(self.sigrow_base as u16, 0x24, LE).unwrap();
        data.pwrite_with(self.fuses_base as u16, 0x26, LE).unwrap();
        data.pwrite_with(self.lock_base as u16, 0x28, LE).unwrap();
        data.pwrite_with(self.device_id as u16, 0x2a, LE).unwrap();
        data.pwrite_with((self.prog_base >> 16) as u8, 0x2c, LE)
            .unwrap();
        data.pwrite_with((self.flash_pages_bytes >> 8) as u8, 0x2d, LE)
            .unwrap();
        data.pwrite_with(self.address_size as u8, 0x2e, LE).unwrap();

        data
    }
}

static avr128da48_device_data: TinyXDeviceData = TinyXDeviceData {
    prog_base: 0x00800000,
    flash_pages_bytes: 0x200,
    eeprom_pages_bytes: 0x01,
    nvmctrl_module_address: 0x00001000,
    ocd_module_address: 0x0F80,
    flash_bytes: 0x20000,
    eeprom_bytes: 0x200,
    user_sig_bytes_bytes: 0x20,
    fuse_bytes: 0x10,
    eeprom_base: 0x1400,
    user_row_base: 0x1080,
    sigrow_base: 0x1100,
    fuses_base: 0x1050,
    lock_base: 0x1040,
    device_id: 0x1E9708,
    address_size: AddressSize::Size24bit,
};

impl std::fmt::Debug for EDBG {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("DAPLink")
            .field("speed_khz", &self.speed_khz)
            .finish()
    }
}

impl EDBG {
    pub fn new_from_device(device: CmsisDapDevice) -> Self {
        log::debug!("Creating new edbg device");

        Self {
            device,
            speed_khz: 100,
            sequence_number: 0,
            protocol: None,
        }
    }

    fn send_command(
        &mut self,
        sub_protocol_id: SubProtocols,
        command_packet: &[u8],
    ) -> Result<Vec<u8>, DebugProbeError> {
        //let report_size = 512;

        let mut packet: Vec<u8> = vec![
            EDBG_SOF,
            0x00,
            (self.sequence_number & 0xff) as u8,
            (self.sequence_number >> 8) as u8,
            sub_protocol_id.clone() as u8,
        ];
        packet.extend_from_slice(command_packet);

        commands::send_command::<AvrCommand>(
            &mut self.device,
            // FIXME: fragment info need to be properly calculated
            AvrCommand {
                fragment_info: 0x11,
                command_packet: packet.as_slice(),
            },
        )?;

        // FIXME: Handle data split accross multiple packages

        let mut response_data: Vec<u8> = vec![];

        let rsp = commands::send_command::<AvrRSPRequest>(&mut self.device, AvrRSPRequest)?;

        log::debug!("Fragment info: {}", rsp.fragment_info);

        let total_fragments: u8 = rsp.fragment_info & 0x0f;
        response_data.extend(&rsp.command_packet);

        for i in 2..(total_fragments + 1) {
            let rsp = commands::send_command::<AvrRSPRequest>(&mut self.device, AvrRSPRequest)?;

            let current_fragment = (rsp.fragment_info & 0xF0) >> 4;
            if rsp.fragment_info == 0 || current_fragment != i {
                panic!("Invalid fragment");
            }
            response_data.extend(&rsp.command_packet);
        }

        if response_data[0] != EDBG_SOF {
            panic!("Wrong SOF byte in AVR RSP");
        }
        if response_data
            .pread_with::<u16>(1, LE)
            .expect("Failed to read buffer")
            != self.sequence_number
        {
            panic!("Wrong sequence number in AVR RSP");
        }

        self.sequence_number += 1;
        response_data.drain(0..4);
        Ok(response_data)
    }

    /// Send a AVR8Generic command. `version` is normaly 0
    fn send_command_avr8_generic(
        &mut self,
        cmd: avr8generic::Commands,
        version: u8,
        data: &[u8],
    ) -> Result<avr8generic::Response, DebugProbeError> {
        log::trace!(
            "Sending avr8generic::Command {:?}, with data:{:02X?}",
            cmd,
            data
        );
        let packet = &[&[cmd as u8, version], data].concat();
        log::trace!("Sending {:x?}", packet);
        let response = self
            .send_command(SubProtocols::AVR8Generic, packet)
            .map(|r| avr8generic::Response::parse_response(&r));

        if let Ok(r) = &response {
            log::trace!("Command response: {:X?}", r);
        }

        response
    }

    fn send_command_housekeeping(
        &mut self,
        cmd: housekeeping::Commands,
        version: u8,
        data: &[u8],
    ) -> Result<housekeeping::Response, DebugProbeError> {
        log::trace!("Sending housekeeping {:?}, with data:{:?}", cmd, data);
        let packet = &[&[cmd as u8, version], data].concat();
        log::trace!("Sending {:x?}", packet);
        let response = self
            .send_command(SubProtocols::Housekeeping, packet)
            .map(|r| housekeeping::Response::parse_response(&r));

        if let Ok(r) = &response {
            log::trace!("Command response: {:?}", r);
        }

        response
    }

    fn check_event(&mut self) -> Result<Vec<u8>, DebugProbeError> {
        let response =
            commands::send_command::<AvrEventRequest>(&mut self.device, AvrEventRequest)?;

        Ok(response.events)
    }

    fn query(
        &mut self,
        sub_protocol: SubProtocols,
        query_context: u8,
    ) -> Result<Vec<u8>, DebugProbeError> {
        self.send_command(sub_protocol, &[0x00, 0x00, query_context])
    }

    /// Discover what sub protocols the probe supports
    fn discover_protocols(&mut self) -> Result<Vec<SubProtocols>, DebugProbeError> {
        let rsp = self.query(
            SubProtocols::Discovery,
            Jtagice3DiscoveryCommands::CmdQuery as u8,
        )?;
        if Jtagice3DiscoveryResponses::RspDiscoveryList as u8 == rsp[0] {
            let mut protocols: Vec<SubProtocols> = Vec::new();
            for p in rsp[2..].iter() {
                protocols.push(SubProtocols::from_u8(*p).unwrap())
            }
            Ok(protocols)
        } else {
            unimplemented!("RSP discovery did not return list");
        }
    }

    fn housekeeping_start_session(&mut self) -> Result<(), DebugProbeError> {
        self.send_command_housekeeping(housekeeping::Commands::StartSession, 0, &[])?;
        Ok(())
    }

    fn avr8generic_set(
        &mut self,
        context: avr8generic::SetGetContexts,
        address: u8,
        data: &[u8],
    ) -> Result<(), DebugProbeError> {
        self.send_command_avr8_generic(
            avr8generic::Commands::Set,
            0x00,
            &[&[context as u8, address, data.len() as u8], data].concat(),
        )?;
        Ok(())
    }

    fn avr8generic_get(
        &mut self,
        context: avr8generic::SetGetContexts,
        address: u8,
        data: &mut [u8],
    ) -> Result<(), DebugProbeError> {
        let response = self.send_command_avr8_generic(
            avr8generic::Commands::Get,
            0x00,
            &[context as u8, address, data.len() as u8],
        )?;
        match response {
            avr8generic::Response::Data(d) => {
                data.copy_from_slice(&d);
                Ok(())
            }
            avr8generic::Response::Failed(f) => Err(EdbgError::ErrorCode(f).into()),
            _ => Err(EdbgError::UnexpectedResponse.into()),
        }
    }

    fn send_device_data(&mut self, device_data: TinyXDeviceData) -> Result<(), DebugProbeError> {
        let data = device_data.to_device_data();

        self.avr8generic_set(avr8generic::SetGetContexts::Device, 0x00, &data)?;
        Ok(())
    }

    pub fn read_program_counter(&mut self) -> Result<u32, DebugProbeError> {
        let response = self.send_command_avr8_generic(avr8generic::Commands::PcRead, 0, &[])?;
        match response {
            avr8generic::Response::Pc(pc) => Ok(pc * 2),
            avr8generic::Response::Failed(f) => Err(EdbgError::ErrorCode(f).into()),
            _ => Err(EdbgError::UnexpectedResponse.into()),
        }
    }

    pub fn read_sreg(&mut self) -> Result<u32, DebugProbeError> {
        let mut data = [0u8; 1];
        self.read_memory(0x1C, &mut data[..], avr8generic::Memtypes::Ocd)?;
        Ok(u8::from_le_bytes(data) as u32)
    }

    pub fn read_stack_pointer(&mut self) -> Result<u32, DebugProbeError> {
        let mut data = [0u8; 2];
        self.read_memory(0x18, &mut data[..], avr8generic::Memtypes::Ocd)?;
        Ok(u16::from_le_bytes(data) as u32)
    }

    fn get_id(&mut self) -> Result<u32, DebugProbeError> {
        let response = self.send_command_avr8_generic(avr8generic::Commands::GetId, 0, &[])?;
        if let avr8generic::Response::Data(data) = response {
            Ok(data.pread_with(0, LE).unwrap())
        } else {
            panic!("Unable to read Program Counter");
        }
    }

    pub fn read_memory(
        &mut self,
        address: u32,
        data: &mut [u8],
        mem_type: avr8generic::Memtypes,
    ) -> Result<(), DebugProbeError> {
        let response = self.send_command_avr8_generic(
            avr8generic::Commands::MemoryRead,
            0,
            &[
                &[mem_type as u8],
                &address.to_le_bytes()[..],
                &data.len().to_le_bytes()[..],
            ]
            .concat(),
        )?;

        match response {
            avr8generic::Response::Data(d) => {
                data.copy_from_slice(&d);
                Ok(())
            }
            avr8generic::Response::Failed(f) => Err(EdbgError::ErrorCode(f).into()),
            _ => Err(EdbgError::UnexpectedResponse.into()),
        }
    }

    pub fn write_memory(
        &mut self,
        address: u32,
        data: &[u8],
        mem_type: avr8generic::Memtypes,
    ) -> Result<(), DebugProbeError> {
        let response = self.send_command_avr8_generic(
            avr8generic::Commands::MemoryWrite,
            0,
            &[
                &[mem_type as u8],
                &address.to_le_bytes()[..],
                &data.len().to_le_bytes()[..],
                &[0], // Write first then reply
                data,
            ]
            .concat(),
        )?;

        match response {
            avr8generic::Response::Ok => Ok(()),
            avr8generic::Response::Failed(f) => Err(EdbgError::ErrorCode(f).into()),
            _ => Err(EdbgError::UnexpectedResponse.into()),
        }
    }
}

impl EDBG {
    // Private functions for core interface
    pub fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error> {
        self.send_command_avr8_generic(
            avr8generic::Commands::HwBreakClear,
            0,
            &[unit_index as u8],
        )?;
        Ok(())
    }
    pub fn status(&mut self) -> Result<CoreStatus, error::Error> {
        let mut data = [0u8; 1];
        self.avr8generic_get(avr8generic::SetGetContexts::Test, 0, &mut data)?;

        if data[0] == 0 {
            Ok(CoreStatus::Halted(HaltReason::Unknown))
        } else {
            Ok(CoreStatus::Running)
        }
    }

    pub fn halt(&mut self, _timeout: Duration) -> Result<CoreInformation, error::Error> {
        // FIXME: Implementation currently ignores timeout argmuent
        self.send_command_avr8_generic(avr8generic::Commands::Stop, 0, &[1])?;
        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }

    pub fn run(&mut self) -> Result<(), error::Error> {
        self.send_command_avr8_generic(avr8generic::Commands::Run, 0, &[])?;
        Ok(())
    }

    pub fn reset_and_halt(&mut self, _timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.send_command_avr8_generic(avr8generic::Commands::Reset, 0, &[1])?;

        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }

    pub fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.send_command_avr8_generic(avr8generic::Commands::Step, 0, &[1, 1])?;

        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }
}

impl DebugProbe for EDBG {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        let selector = selector.into();
        log::debug!("Attemting to open EDBG device {:?}", selector);
        let device = cmsisdap::tools::open_device_from_selector(selector)?;
        let mut probe = Self::new_from_device(device);

        let protocols = probe.discover_protocols()?;
        log::debug!("Found protocols {:?}", protocols);
        probe.housekeeping_start_session()?;

        Ok(Box::new(probe))
    }

    fn get_name(&self) -> &str {
        "EDBG"
    }

    /// Check if the probe offers an interface to debug AVR chips.
    fn has_avr_interface(&self) -> bool {
        true
    }

    fn try_get_avr_interface(
        self: Box<Self>,
    ) -> Result<AvrCommunicationInterface, (Box<dyn DebugProbe>, DebugProbeError)> {
        match AvrCommunicationInterface::new(self) {
            Ok(interface) => Ok(interface),
            Err((probe, err)) => Err((probe.into_probe(), err)),
        }
    }

    fn speed(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        //FIXME: Check if speed is valid
        self.speed_khz = speed_khz;

        Ok(self.speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Running attach");
        self.housekeeping_start_session()?;

        self.select_protocol(WireProtocol::Avr(AvrWireProtocol::Updi))?;

        self.send_device_data(avr128da48_device_data)?;

        self.send_command_avr8_generic(avr8generic::Commands::ActivatePhysical, 0, &[0])?;
        self.send_command_avr8_generic(avr8generic::Commands::Attach, 0, &[0])?;
        Ok(())
    }

    fn detach(&mut self) -> Result<(), DebugProbeError> {
        self.send_command_avr8_generic(avr8generic::Commands::Detach, 0, &[0])?;
        Ok(())
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        log::debug!("Attemting to select protocol: {:?}", protocol);
        if let WireProtocol::Avr(AvrWireProtocol::Updi) = protocol {
            // Disable high voltage
            self.avr8generic_set(
                avr8generic::SetGetContexts::Options,
                avr8generic::OptionsContextParameters::HvUpdiEnable as u8,
                &[0],
            )?;

            self.avr8generic_set(
                avr8generic::SetGetContexts::Config,
                avr8generic::ConfigContextParameters::Variant as u8,
                &[avr8generic::VariantValues::Updi as u8],
            )?;

            // Select debug functionality
            self.avr8generic_set(
                avr8generic::SetGetContexts::Config,
                avr8generic::ConfigContextParameters::Function as u8,
                &[avr8generic::FunctionValues::Debugging as u8],
            )?;

            self.avr8generic_set(
                avr8generic::SetGetContexts::Physical,
                avr8generic::PhysicalContextParameters::Interface as u8,
                &[avr8generic::PhysicalInterfaces::UPDI as u8],
            )?;

            self.avr8generic_set(
                avr8generic::SetGetContexts::Physical,
                avr8generic::PhysicalContextParameters::XmPdiClK as u8,
                &(self.speed_khz as u16).to_le_bytes(),
            )?;
        } else {
            return Err(DebugProbeError::NotImplemented(
                "Only UPDI is implemented for AVR EDBG",
            ));
        }

        Ok(())
    }
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        self.send_command_avr8_generic(avr8generic::Commands::Reset, 0, &[1])?;
        Ok(())
    }

    fn target_reset_assert(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!();
    }
    fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn into_probe(self: Box<Self>) -> Box<dyn DebugProbe> {
        self
    }
}
