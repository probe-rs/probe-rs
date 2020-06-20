use super::super::memory::romtable::Component;
use crate::{Core, Error};

pub const _TPIU_PID: [u8; 8] = [0xA1, 0xB9, 0x0B, 0x0, 0x4, 0x0, 0x0, 0x0];

const _REGISTER_OFFSET_TPIU_SSPSR: u32 = 0x0;
const REGISTER_OFFSET_TPIU_CSPSR: u32 = 0x4;
const REGISTER_OFFSET_TPIU_ACPR: u32 = 0x10;
const REGISTER_OFFSET_TPIU_SPPR: u32 = 0xF0;
const REGISTER_OFFSET_TPIU_FFCR: u32 = 0x304;

/// TPIU unit
///
/// Trace port interface unit unit.
pub struct Tpiu<'probe: 'core, 'core> {
    component: &'core Component,
    core: &'core mut Core<'probe>,
}

impl<'probe: 'core, 'core> Tpiu<'probe, 'core> {
    pub fn new(core: &'core mut Core<'probe>, component: &'core Component) -> Self {
        Tpiu { core, component }
    }

    pub fn set_port_size(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_TPIU_CSPSR, value)?;
        Ok(())
    }

    pub fn set_prescaler(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_TPIU_ACPR, value)?;
        Ok(())
    }

    /// Set protocol.
    /// 0 = sync trace mode
    /// 1 = async SWO (manchester)
    /// 2 = async SWO (NRZ)
    /// 3 = reserved
    pub fn set_pin_protocol(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_TPIU_SPPR, value)?;
        Ok(())
    }

    pub fn set_formatter(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_TPIU_FFCR, value)?;
        Ok(())
    }
}
