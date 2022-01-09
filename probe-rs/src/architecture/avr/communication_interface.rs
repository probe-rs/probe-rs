use crate::error;
use crate::DebugProbeError;
use crate::Error as ProbeRsError;
use thiserror::Error;

use crate::{
    CoreInformation, CoreInterface, CoreStatus,
};

use std::time::Duration;

use crate::probe::cmsisdap::CmsisDap;
use crate::probe::edbg::avr8generic;
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

impl<'probe> AvrCommunicationInterface {
    pub fn new(probe: Box<EDBG>) -> Result<Self, (Box<CmsisDap>, DebugProbeError)> {
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
impl<'probe> AvrCommunicationInterface {
    pub fn clear_breakpoint(&mut self, unit_index: usize) -> Result<(), error::Error> {
        self.probe.as_mut().clear_breakpoint(unit_index)
    }

    pub fn halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.probe.as_mut().halt(timeout)
    }

    pub fn run(&mut self) -> Result<(), error::Error> {
        self.probe.as_mut().run()
    }

    pub fn reset_and_halt(&mut self, timeout: Duration) -> Result<CoreInformation, error::Error> {
        self.probe.as_mut().reset_and_halt(timeout)
    }

    pub fn step(&mut self) -> Result<CoreInformation, error::Error> {
        self.probe.as_mut().step()
    }

    // Memory interface
    pub fn read_word_8(&mut self, address: u32) -> Result<u8, error::Error> {
        let mut data = [0u8; 1];
        self.probe
            .as_mut()
            .read_memory(address, &mut data[..], avr8generic::Memtypes::Sram)?;
        Ok(data[0])
    }

    pub fn read_8(&mut self, address: u32, data: &mut [u8]) -> Result<(), error::Error> {
        let addr_space = (address & 0x00ff0000) >> 16;
        let mem_type = match addr_space{
            0x00 => avr8generic::Memtypes::ApplFlash,
            0x80 => avr8generic::Memtypes::Sram,
            0x81 => avr8generic::Memtypes::Eeprom,
            0x82 => avr8generic::Memtypes::Fuses,
            0x83 => avr8generic::Memtypes::Lockbits,
            0x84 => avr8generic::Memtypes::Signature,
            0x85 => avr8generic::Memtypes::UserSignature,
            _ => unimplemented!("Unknown AVR memory type"),
        };
        self.probe
            .as_mut()
            .read_memory(address, &mut data[..], mem_type)?;
        Ok(())
    }

    pub fn write_word_8(&mut self, address: u32, data: u8) -> Result<(), error::Error> {
        self.probe
            .as_mut()
            .write_memory(address, &[data], avr8generic::Memtypes::Sram)?;
        Ok(())
    }

    pub fn read_register(&mut self, address: u32) -> Result<u8, error::Error> {
        Ok(self.read_registerfile()?[address as usize])
    }

    pub fn status(&mut self) -> Result<CoreStatus, error::Error> {
        self.probe.as_mut().status()
    }

    pub fn read_registerfile(&mut self) -> Result<Vec<u8>, error::Error> {
        let mut data = vec![0u8; 32];
        self.probe
            .as_mut()
            .read_memory(0, &mut data[..], avr8generic::Memtypes::Regfile)?;
        Ok(data)
    }

    pub fn read_sreg(&mut self) -> Result<u32, error::Error> {
        Ok(self.probe.as_mut().read_sreg()?)
    }

    pub fn read_program_counter(&mut self) -> Result<u32, error::Error> {
        Ok(self.probe.as_mut().read_program_counter()?)
    }

    pub fn read_stack_pointer(&mut self) -> Result<u32, error::Error> {
        Ok(self.probe.as_mut().read_stack_pointer()?)
    }
}
