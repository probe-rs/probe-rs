use crate::DebugProbeError;
use crate::Error as ProbeRsError;
use thiserror::Error;

use crate::core::Architecture;

use crate::DebugProbe;
use crate::probe::daplink::DAPLink;
use crate::probe::daplink::{
    commands,
    commands::edbg::{
        avr_cmd::{AvrCommand, AvrCommandResponse},
        avr_rsp::{AvrRSPRequest, AvrRSPResponse},
        avr_evt::{AvrEventRequest, AvrEventResponse},
    },
};

#[derive(Debug, Error)]
pub enum AvrEdbgError {
    #[error("Unexpected answer to avr command")]
    UnexpectedAnswer,
    #[error("Debug Probe Error")]
    DebugProbe(#[from] DebugProbeError),
}

impl From<AvrEdbgError> for ProbeRsError {
    fn from(err: AvrEdbgError) -> Self {
        match err {
            AvrEdbgError::DebugProbe(e) => e.into(),
            other => ProbeRsError::ArchitectureSpecific(Box::new(other)),
        }
    }
}

#[derive(Debug)]
pub struct AvrCommunicationInterface {
    probe: Box<DAPLink>,
}

impl AvrCommunicationInterface {
    fn send_command(&mut self, command_packet: &[u8]) -> Result<Vec<u8>, DebugProbeError> {
        let report_size = 512;
        commands::send_command::<AvrCommand, AvrCommandResponse>(
            &mut self.probe.device,
            // FIXME: fragment info need to be properly calculated
            AvrCommand {
                fragment_info: 0x11,
                command_packet,
            },
        )?;

        // FIXME: Handle data split accross multiple packages
        let rsp = loop {
            let rsp = commands::send_command::<AvrRSPRequest, AvrRSPResponse>(
                &mut self.probe.device,
                AvrRSPRequest,
            )?;

            if rsp.fragment_info != 0 {
                break rsp;
            }
        };
        Ok(rsp.command_packet)
    }

    fn check_event(&mut self) -> Result<Vec<u8>, DebugProbeError> {
        let response = commands::send_command::<AvrEventRequest, AvrEventResponse>(
            &mut self.probe.device,
            AvrEventRequest)?;

        Ok(response.events)
    }
}

// Edbg part
impl AvrCommunicationInterface {
}

impl<'a> AsRef<dyn DebugProbe + 'a> for AvrCommunicationInterface {
    fn as_ref(&self) -> &(dyn DebugProbe + 'a) {
        self.probe.as_ref().as_ref()
    }
}

impl<'a> AsMut<dyn DebugProbe + 'a> for AvrCommunicationInterface {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'a) {
        self.probe.as_mut().as_mut()
    }
}
