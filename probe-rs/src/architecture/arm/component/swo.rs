use super::super::memory::romtable::Component;
use crate::{Core, Error};

const REGISTER_OFFSET_SWO_CODR: u32 = 0x10;
const REGISTER_OFFSET_SWO_SPPR: u32 = 0xF0;
const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

/// SWO unit
///
/// Serial Wire Output unit.
pub struct Swo<'probe: 'core, 'core> {
    component: &'core Component,
    core: &'core mut Core<'probe>,
}

impl <'probe: 'core, 'core> Swo<'probe, 'core> {
    /// Construct a new SWO component.
    pub fn new(core: &'core mut Core<'probe>, component: &'core Component) -> Self {
        Swo { component, core }
    }

    /// Unlock the SWO and enable it for tracing the target.
    ///
    /// This function enables the SWOunit as a whole. It does not actually send any data after enabling it.
    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    /// Set the prescaler of the SWO.
    pub fn set_prescaler(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_SWO_CODR, value)?;
        Ok(())
    }

    /// Set the SWO protocol.
    /// 0 = sync trace mode
    /// 1 = async SWO (manchester)
    /// 2 = async SWO (NRZ)
    /// 3 = reserved
    pub fn set_pin_protocol(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_SWO_SPPR, value)?;
        Ok(())
    }
}
