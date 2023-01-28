//! Arm SWO CoreSight Component
//!
//! # Description
//! This module provides access and control of the SWO CoreSight component block.
use super::super::memory::romtable::CoresightComponent;
use crate::architecture::arm::ArmProbeInterface;
use crate::Error;

const REGISTER_OFFSET_SWO_CODR: u32 = 0x10;
const REGISTER_OFFSET_SWO_SPPR: u32 = 0xF0;
const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

/// SWO unit
///
/// Serial Wire Output unit.
pub struct Swo<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> Swo<'a> {
    /// Construct a new SWO component.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Swo {
            component,
            interface,
        }
    }

    /// Unlock the SWO and enable it for tracing the target.
    ///
    /// This function enables the SWO unit as a whole. It does not actually send any data after
    /// enabling it.
    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    /// Set the prescaler of the SWO.
    pub fn set_prescaler(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_SWO_CODR, value)?;
        Ok(())
    }

    /// Set the SWO protocol.
    /// 0 = sync trace mode
    /// 1 = async SWO (manchester)
    /// 2 = async SWO (NRZ)
    /// 3 = reserved
    pub fn set_pin_protocol(&mut self, value: u32) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_SWO_SPPR, value)?;
        Ok(())
    }
}
