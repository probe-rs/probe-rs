//! Arm trace funnel CoreSight Component
//!
//! # Description
//! This module provides access and control of the trace funnel CoreSight component block.
use super::DebugRegister;
use crate::architecture::arm::memory::romtable::CoresightComponent;
use crate::architecture::arm::ArmProbeInterface;
use crate::Error;
use bitfield::bitfield;

const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

/// Trace funnel unit
pub struct TraceFunnel<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut Box<dyn ArmProbeInterface>,
}

impl<'a> TraceFunnel<'a> {
    /// Construct a new SWO component.
    pub fn new(
        interface: &'a mut Box<dyn ArmProbeInterface>,
        component: &'a CoresightComponent,
    ) -> Self {
        TraceFunnel {
            component,
            interface,
        }
    }

    /// Unlock the SWO and enable it for tracing the target.
    ///
    /// This function enables the SWOunit as a whole. It does not actually send any data after enabling it.
    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    /// Enable funnel input sources.
    ///
    /// # Note
    /// The trace funnel acts as a selector for multiple sources. This function allows you to block
    /// or pass specific trace sources selectively.
    pub fn enable_port(&mut self, mask: u8) -> Result<(), Error> {
        let mut control = Control::load(self.component, self.interface)?;
        control.set_slave_enable(mask);
        control.store(self.component, self.interface)
    }
}

bitfield! {
    #[derive(Clone, Default)]
    pub struct Control(u32);
    impl Debug;
    pub u8, min_hold_time, set_min_hold_time: 11, 8;
    pub u8, enable_slave_port, set_slave_enable: 7, 0;
}

impl DebugRegister for Control {
    const ADDRESS: u32 = 0x00;
    const NAME: &'static str = "CSTF/CTRL";
}

impl From<u32> for Control {
    fn from(raw: u32) -> Control {
        Control(raw)
    }
}

impl From<Control> for u32 {
    fn from(control: Control) -> u32 {
        control.0
    }
}
