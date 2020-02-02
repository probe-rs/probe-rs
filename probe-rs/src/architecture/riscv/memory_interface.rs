use crate::DebugProbeError;
use crate::MemoryInterface;
use crate::Probe;

pub struct RiscvMemoryInterface {
    probe: Probe,
}

impl RiscvMemoryInterface {
    pub fn new(probe: Probe, index: usize) -> Result<Self, DebugProbeError> {
        Ok(RiscvMemoryInterface { probe })
    }
}

impl MemoryInterface for RiscvMemoryInterface {

    fn read32(&mut self, address: u32) -> Result<u32, crate::Error> {
        unimplemented!()
    }
    fn read8(&mut self, address: u32) -> Result<u8, crate::Error> {
        unimplemented!()
    }
    fn read_block32(&mut self, address: u32, data: &mut [u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn read_block8(&mut self, address: u32, data: &mut [u8]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write32(&mut self, addr: u32, data: u32) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write8(&mut self, addr: u32, data: u8) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block32(&mut self, addr: u32, data: &[u32]) -> Result<(), crate::Error> {
        unimplemented!()
    }
    fn write_block8(&mut self, addr: u32, data: &[u8]) -> Result<(), crate::Error> {
        unimplemented!()
    }
}
