use crate::error;
use log::debug;
use scroll::{Pread, Pwrite, LE};

use crate::architecture::avr::communication_interface::AvrCommunicationInterface;
use crate::probe::cmsisdap;
use crate::probe::cmsisdap::commands;
use crate::probe::cmsisdap::commands::edbg::{
    avr_cmd::AvrCommand, avr_cmd::AvrCommandResponse, avr_evt::AvrEventRequest,
    avr_evt::AvrEventResponse, avr_rsp::AvrRSPRequest, avr_rsp::AvrRSPResponse,
};
use crate::probe::cmsisdap::commands::CmsisDapDevice;
use crate::probe::cmsisdap::CmsisDap;
use crate::DebugProbe;
use crate::DebugProbeError;
use crate::DebugProbeSelector;
use crate::WireProtocol;
use crate::{CoreInformation, CoreInterface, CoreRegisterAddress, CoreStatus, MemoryInterface};
use enum_primitive_derive::Primitive;
use num_traits::FromPrimitive;

use std::time::Duration;

use std::{convert::TryFrom, fmt};

mod avr8generic;
use avr8generic::*;

mod housekeeping;

pub mod tools;

pub struct EDBG {
    pub device: CmsisDapDevice,
    pub speed_khz: u32,
    pub sequence_number: u16,
    pub avr8generic_protocol: Option<Avr8GenericProtocol>,
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

#[derive(Clone, Debug)]
pub enum Avr8GenericResponse {
    Ok,
    List(Vec<u8>),
    Data(Vec<u8>),
    Pc(u32),
    Failed(Avr8GenericFailureCodes),
}

impl Avr8GenericResponse {
    fn parse_response(response: &[u8]) -> Self {
        match Avr8GenericResponses::from_u8(response[0]).unwrap() {
            Avr8GenericResponses::StatusOk => Avr8GenericResponse::Ok,
            Avr8GenericResponses::List => Avr8GenericResponse::List(response[2..].to_vec()),
            Avr8GenericResponses::Data => {
                if *response.last().expect("No status in response") == 0x00 {
                    Avr8GenericResponse::Data(response[2..response.len() - 1].to_vec())
                } else {
                    Avr8GenericResponse::Failed(Avr8GenericFailureCodes::Unknown)
                }
            }
            Avr8GenericResponses::Pc => Avr8GenericResponse::Pc(
                response
                    .pread_with::<u32>(2, LE)
                    .expect("Unable to read PC"),
            ),
            Avr8GenericResponses::Failed => Avr8GenericResponse::Failed(
                Avr8GenericFailureCodes::from_u8(response[2])
                    .expect("Unable to find matching error code"),
            ),
        }
    }
}

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
        log::debug!("Createing new edbg device");

        Self {
            device,
            speed_khz: 100,
            sequence_number: 0,
            avr8generic_protocol: None,
        }
    }

    fn send_command(
        &mut self,
        sub_protocol_id: SubProtocols,
        command_packet: &[u8],
    ) -> Result<Vec<u8>, DebugProbeError> {
        let report_size = 512;

        let mut packet: Vec<u8> = vec![
            EDBG_SOF,
            0x00,
            (self.sequence_number & 0xff) as u8,
            (self.sequence_number >> 8) as u8,
            sub_protocol_id.clone() as u8,
        ];
        packet.extend_from_slice(command_packet);

        commands::send_command::<AvrCommand, AvrCommandResponse>(
            &mut self.device,
            // FIXME: fragment info need to be properly calculated
            AvrCommand {
                fragment_info: 0x11,
                command_packet: packet.as_slice(),
            },
        )?;

        // FIXME: Handle data split accross multiple packages
        let mut rsp = loop {
            let rsp = commands::send_command::<AvrRSPRequest, AvrRSPResponse>(
                &mut self.device,
                AvrRSPRequest,
            )?;

            if rsp.fragment_info != 0 {
                break rsp;
            }
        };

        // FIXME: use propper errors
        if rsp.command_packet[0] != EDBG_SOF {
            panic!("Wrong SOF byte in AVR RSP");
        }
        if rsp
            .command_packet
            .pread_with::<u16>(1, LE)
            .expect("Failed to read buffer")
            != self.sequence_number
        {
            panic!("Wrong sequence number in AVR RSP");
        }
        //if rsp.command_packet[3] != sub_protocol_id as u8 {
        //    panic!("Wrong sub protocol in AVR RSP");
        //}
        self.sequence_number += 1;
        rsp.command_packet.drain(0..4);
        Ok(rsp.command_packet)
    }

    /// Send a AVR8Generic command. `version` is normaly 0
    fn send_command_avr8_generic(
        &mut self,
        cmd: Avr8GenericCommands,
        version: u8,
        data: &[u8],
    ) -> Result<Avr8GenericResponse, DebugProbeError> {
        log::trace!(
            "Sending Avr8GenericCommand {:?}, with data:{:02X?}",
            cmd,
            data
        );
        let packet = &[&[cmd as u8, version], data].concat();
        log::trace!("Sending {:x?}", packet);
        let response = self
            .send_command(SubProtocols::AVR8Generic, packet)
            .map(|r| Avr8GenericResponse::parse_response(&r));

        if let Ok(r) = &response {
            log::trace!("Command response: {:?}", r);
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
        let response = commands::send_command::<AvrEventRequest, AvrEventResponse>(
            &mut self.device,
            AvrEventRequest,
        )?;

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
        context: Avr8GenericSetGetContexts,
        address: u8,
        data: &[u8],
    ) -> Result<(), DebugProbeError> {
        self.send_command_avr8_generic(
            Avr8GenericCommands::Set,
            0x00,
            &[&[context as u8, address, data.len() as u8], data].concat(),
        )?;
        Ok(())
    }

    fn send_device_data(&mut self, device_data: TinyXDeviceData) -> Result<(), DebugProbeError> {
        let data = device_data.to_device_data();

        self.avr8generic_set(Avr8GenericSetGetContexts::Device, 0x00, &data)?;
        Ok(())
    }

    fn read_program_counter(&mut self) -> Result<u32, DebugProbeError> {
        let response = self.send_command_avr8_generic(Avr8GenericCommands::PcRead, 0, &[])?;
        if let Avr8GenericResponse::Pc(pc) = response {
            Ok(pc)
        } else {
            panic!("Unable to read Program Counter");
        }
    }
}

impl EDBG {
    // Private functions for core interface
    pub fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error> {
        self.send_command_avr8_generic(Avr8GenericCommands::HwBreakClear, 0, &[unit_index as u8])?;
        Ok(())
    }

    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        // FIXME: Implementation currently ignores timeout argmuent
        self.send_command_avr8_generic(Avr8GenericCommands::Stop, 0, &[1])?;
        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }

    pub fn run(&mut self) -> Result<(), error::Error> {
        self.send_command_avr8_generic(Avr8GenericCommands::Run, 0, &[])?;
        Ok(())
    }

    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.send_command_avr8_generic(Avr8GenericCommands::Reset, 0, &[1])?;

        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }

    pub fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.send_command_avr8_generic(Avr8GenericCommands::Step, 0, &[1, 1])?;

        let pc = self.read_program_counter()?;

        Ok(CoreInformation { pc })
    }
    pub fn read_word_8(&mut self, address: u32) -> Result<u8, error::Error> {
        let mem_type = 0xC0;
        let data = self.send_command_avr8_generic(
            Avr8GenericCommands::MemoryRead,
            0,
            &[
                &[mem_type],
                &address.to_le_bytes()[..],
                &2u32.to_le_bytes()[..],
            ]
            .concat(),
        )?;
        if let Avr8GenericResponse::Data(d) = data {
            Ok(d[0])
        } else {
            unimplemented!("No data recived from debugger");
        }
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
        probe.housekeeping_start_session()?;
        log::debug!("Found protocols {:?}", protocols);

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
        todo!("Set speed not done");

        //        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        log::debug!("Running attach");
        self.housekeeping_start_session()?;

        self.select_protocol(WireProtocol::Avr(AvrWireProtocol::Updi))?;

        self.send_device_data(avr128da48_device_data)?;

        self.send_command_avr8_generic(Avr8GenericCommands::ActivatePhysical, 0, &[0])?;
        self.send_command_avr8_generic(Avr8GenericCommands::Attach, 0, &[0])?;
        Ok(())
    }

    fn detach(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        log::debug!("Attemting to select protocol: {:?}", protocol);
        // Disable high voltage
        self.avr8generic_set(
            Avr8GenericSetGetContexts::Options,
            Avr8GenericOptionsContextParameters::HvUpdiEnable as u8,
            &[0],
        )?;

        self.avr8generic_set(
            Avr8GenericSetGetContexts::Config,
            Avr8GenericConfigContextParameters::Variant as u8,
            &[Avr8GenericVariantValues::Updi as u8],
        )?;

        // Select debug functionality
        self.avr8generic_set(
            Avr8GenericSetGetContexts::Config,
            Avr8GenericConfigContextParameters::Function as u8,
            &[Avr8GenericFunctionValues::Debugging as u8],
        )?;

        self.avr8generic_set(
            Avr8GenericSetGetContexts::Physical,
            Avr8GenericPhysicalContextParameters::Interface as u8,
            &[Avr8GenericPhysicalInterfaces::UPDI as u8],
        )?;

        self.avr8generic_set(
            Avr8GenericSetGetContexts::Physical,
            Avr8GenericPhysicalContextParameters::XmPdiClK as u8,
            &(self.speed_khz as u16).to_le_bytes(),
        )?;

        Ok(())
    }
    fn target_reset(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!();
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
