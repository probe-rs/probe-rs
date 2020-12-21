use log::debug;
use scroll::{Pread, LE};
use std::sync::Mutex;

use enum_primitive_derive::Primitive;
use num_traits::FromPrimitive;

use crate::probe::daplink::commands;
use crate::probe::daplink::commands::edbg::{
    avr_cmd::AvrCommand, avr_cmd::AvrCommandResponse, avr_evt::AvrEventRequest,
    avr_evt::AvrEventResponse, avr_rsp::AvrRSPRequest, avr_rsp::AvrRSPResponse,
};
use crate::probe::daplink::commands::DAPLinkDevice;
use crate::probe::daplink::tools;
use crate::DebugProbe;
use crate::DebugProbeError;
use crate::DebugProbeSelector;
use crate::WireProtocol;

mod avr8generic;
use avr8generic::*;

pub struct EDBGprobe {
    pub device: Mutex<DAPLinkDevice>,
    pub speed_khz: u32,
    pub sequence_number: u16,
    pub avr8generic_protocol: Option<Avr8GenericProtocol>,
}

#[derive(Clone, Copy, Debug)]
enum Jtagice3DiscoveryCommands {
    CmdQuery = 0x00,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Jtagice3DiscoveryResponses {
    RspDiscoveryList = 0x81,
    RspDiscoveryFailed = 0xA0,
}

#[derive(Clone, Copy, Debug)]
enum Jtagice3FailureCodes {
    FailureOk = 0x00,
    FailureUsbPrevoiusUnderrun = 0xE0,
    FailureUnknown = 0xFF,
}

#[derive(Clone, Copy, Debug)]
enum Jtagice3Discovery {
    DiscoveryCommandHandlers = 0x00,
    DiscoveryToolName = 0x80,
    DiscoverySerialNumber = 0x81,
    DiscoveryMfnDate = 0x82,
}

#[derive(Clone, Copy, Debug)]
enum Jtagice3DiscoveryFailureCodes {
    DiscoveryFailedNotSupported = 0x10,
}

#[derive(Clone, Copy, Debug, PartialEq, Primitive)]
enum Jtagice3HousekeepingCommands {
    HousekeepingQuery = 0x00,
    HousekeepingSet = 0x01,
    HousekeepingGet = 0x02,
    HousekeepingStartSession = 0x10,
    HousekeepingEndSession = 0x11,
    HousekeepingJtagDetect = 0x30,
    HousekeepingJtagCalOsc = 0x31,
    HousekeepingJtagFwUpgrade = 0x50,
}


const EDBG_SOF: u8 = 0x0E;

#[derive(Clone, Copy, Debug, PartialEq, Primitive)]
enum SubProtocols {
    Discovery = 0x00,
    Housekeeping = 0x01,
    AVRISP = 0x11,
    AVR8Generic = 0x12,
    AVR32Generic = 0x13,
    TPI = 0x14,
    EDBGCtrl = 0x20,
}

impl std::fmt::Debug for EDBGprobe {
    fn fmt(&self, fmt: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt.debug_struct("DAPLink")
            .field("speed_khz", &self.speed_khz)
            .finish()
    }
}

impl EDBGprobe {
    pub fn new_from_device(device: DAPLinkDevice) -> Self {
        log::debug!("Createing new edbg device");

        Self {
            device: Mutex::new(device),
            speed_khz: 1_000,
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
        self.send_command(
            SubProtocols::Housekeeping,
            &[
                Jtagice3HousekeepingCommands::HousekeepingStartSession as u8,
                0x00,
            ],
        )?;
        Ok(())
    }

    fn avr8generic_set(
        &mut self,
        context: Avr8GenericSetGetContexts,
        address: u8,
        data: &[u8],
    ) -> Result<(), DebugProbeError> {
        self.send_command(
            SubProtocols::AVR8Generic,
            &[
                &[
                    Avr8GenericCommands::Set as u8,
                    0x00,
                    context as u8,
                    address,
                    data.len() as u8,
                ],
                data,
            ]
            .concat(),
        )?;

        Ok(())
    }
}

impl DebugProbe for EDBGprobe {
    fn new_from_selector(
        selector: impl Into<DebugProbeSelector>,
    ) -> Result<Box<Self>, DebugProbeError>
    where
        Self: Sized,
    {
        let device = tools::open_device_from_selector(selector)?;
        let mut probe = Self::new_from_device(device);

        let protocols = probe.discover_protocols()?;
        log::debug!("Found protocols {:?}", protocols);

        Ok(Box::new(probe))
    }

    fn get_name(&self) -> &str {
        "EDBG"
    }

    fn speed(&self) -> u32 {
        self.speed_khz
    }

    fn set_speed(&mut self, speed_khz: u32) -> Result<u32, DebugProbeError> {
        todo!("Set speed not done");

        //        Ok(speed_khz)
    }

    fn attach(&mut self) -> Result<(), DebugProbeError> {
        self.housekeeping_start_session()?;
        unimplemented!("Attach not implemented");
    }

    fn detach(&mut self) -> Result<(), DebugProbeError> {
        unimplemented!();
    }

    fn select_protocol(&mut self, protocol: WireProtocol) -> Result<(), DebugProbeError> {
        unimplemented!();
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
}
