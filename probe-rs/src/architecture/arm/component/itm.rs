//! Module for using the ITM.
//!
//! ITM = Instrumentation Trace Macrocell

use super::super::memory::romtable::Component;
use crate::{Core, Error};

pub const _ITM_PID: [u8; 8] = [0x1, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];

pub struct Itm<'c> {
    component: &'c Component,
    core: &'c mut Core,
}

const REGISTER_OFFSET_ITM_TER: usize = 0xE00;
const _REGISTER_OFFSET_ITM_TPR: usize = 0xE40;
const REGISTER_OFFSET_ITM_TCR: usize = 0xE80;
const REGISTER_OFFSET_ACCESS: usize = 0xFB0;

impl<'c> Itm<'c> {
    pub fn new(core: &'c mut Core, component: &'c Component) -> Self {
        Itm { core, component }
    }

    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    pub fn tx_enable(&mut self) -> Result<(), Error> {
        let mut value = self
            .component
            .read_reg(self.core, REGISTER_OFFSET_ITM_TCR)?;
        log::info!("ITM_TCR Before: 0x{:08X}", value);

        value |= 1; // itm enable
        value |= 1 << 1; // timestamp enable
        value |= 1 << 2; // Enable sync pulses, note DWT_CTRL.SYNCTAP must be configured.
        value |= 1 << 3; // tx enable (for DWT)
        value |= 13 << 16; // 7 bits trace bus ID
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ITM_TCR, value)?;

        let value = self
            .component
            .read_reg(self.core, REGISTER_OFFSET_ITM_TCR)?;
        log::info!("ITM_TCR After: 0x{:08X}", value);

        // Enable 32 channels:
        self.component
            .write_reg(self.core, REGISTER_OFFSET_ITM_TER, 0xFFFF_FFFF)?;

        Ok(())
    }
}
