use crate::error;
use crate::DebugProbeError;
use crate::Error as ProbeRsError;
use thiserror::Error;

use crate::{
    Architecture, CoreInformation, CoreInterface, CoreRegisterAddress, CoreStatus, MemoryInterface,
};

use std::time::Duration;


use crate::probe::cmsisdap::CMSISDAP;
use crate::probe::cmsisdap::{
    commands,
    commands::edbg::{
        avr_cmd::{AvrCommand, AvrCommandResponse},
        avr_evt::{AvrEventRequest, AvrEventResponse},
        avr_rsp::{AvrRSPRequest, AvrRSPResponse},
    },
};
use crate::probe::edbg::EDBG;
use crate::DebugProbe;
use crate::Probe;

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
    //probe: Box<CMSISDAP>,
    probe: Box<EDBG>,
}

impl AvrCommunicationInterface {
    pub fn new(probe: Box<EDBG>) -> Result<Self, (Box<CMSISDAP>, DebugProbeError)> {
        Ok(AvrCommunicationInterface { probe })
    }

    pub fn target_reset_deassert(&mut self) -> Result<(), DebugProbeError> {
        //self.dtm.target_reset_deassert()
        unimplemented!()
    }

    pub fn close(self) -> Probe {
        Probe::from_attached_probe(self.probe.into_probe())
    }
}

//Functions for core interface
impl AvrCommunicationInterface {
    pub fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error> {
        self.probe.as_mut().clear_breakpoint(unit_index)
    }

    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.probe.as_mut().halt(timeout)
    }
}
/*
impl<'a> AsRef<dyn DebugProbe + 'a> for AvrCommunicationInterface {
    fn as_ref(&self) -> &(dyn DebugProbe + 'a) {
        self.probe.as_ref()
    }
}

impl<'a> AsMut<dyn DebugProbe + 'a> for AvrCommunicationInterface {
    fn as_mut(&mut self) -> &mut (dyn DebugProbe + 'a) {
        self.probe.as_mut()
    }
}
*/
