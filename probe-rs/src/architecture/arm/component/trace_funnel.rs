//! Arm trace funnel CoreSight Component
//!
//! # Description
//! This module provides access and control of the trace funnel CoreSight component block.
use super::DebugRegister;
use crate::architecture::arm::memory::romtable::CoresightComponent;
use crate::architecture::arm::{ArmError, ArmProbeInterface};
use bitfield::bitfield;

const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

/// Trace funnel unit
pub struct TraceFunnel<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> TraceFunnel<'a> {
    /// Construct a new TraceFunnel component.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        TraceFunnel {
            component,
            interface,
        }
    }

    /// Unlock the funnel and enable it for tracing the target.
    pub fn unlock(&mut self) -> Result<(), ArmError> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    /// Enable funnel input sources.
    ///
    /// # Note
    /// The trace funnel acts as a selector for multiple sources. This function allows you to block
    /// or pass specific trace sources selectively.
    pub fn enable_port(&mut self, mask: u8) -> Result<(), ArmError> {
        let mut control = Control::load(self.component, self.interface)?;
        control.set_slave_enable(mask);
        control.store(self.component, self.interface)
    }
}

bitfield! {
    /// The control register is described in "DDI0314H CoreSight Components Technical Reference
    /// Manual" on page 7-5.
    #[derive(Clone, Default)]
    pub struct Control(u32);
    impl Debug;

    /// The minimum hold time specifics the number of transactions that the arbiter of the funnel
    /// will perform on individual active input before muxing to the next port. The arbiter uses a
    /// round-robin topology for all enabled funnel inputs.
    pub u8, min_hold_time, set_min_hold_time: 11, 8;

    /// The slave enable port specifies a bitfield which indicates which trace funnel input ports
    /// are enabled.
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
