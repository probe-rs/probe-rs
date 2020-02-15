//! Interface with the DWT (data watchpoint and trace) unit.
//!
//! This unit can monitor specific memory locations for write / read
//! access, this could be handy to debug a system :).
//!
//! See ARMv7-M architecture reference manual C1.8 for some additional
//! info about this stuff.

use super::super::memory::romtable::RomTableEntry;
use crate::{Core, Error};

pub const DWT_PID: [u8; 8] = [0x2, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];

/// A struct representing a DWT unit on target.
pub struct Dwt<'c> {
    component: &'c RomTableEntry,
    core: &'c mut Core,
}

const REG_OFFSET_DWT_CTRL: usize = 0;

impl<'c> Dwt<'c> {
    pub fn new(core: &'c mut Core, component: &'c RomTableEntry) -> Self {
        Dwt { core, component }
    }

    pub fn info(&mut self) -> Result<(), Error> {
        let ctrl = self.component.read_reg(self.core, REG_OFFSET_DWT_CTRL)?;

        let num_comparators_available: u8 = ((ctrl >> 28) & 0xf) as u8;
        let has_trace_sampling_support = ctrl & (1 << 27) == 0;
        let has_compare_match_support = ctrl & (1 << 26) == 0;
        let has_cyccnt_support = ctrl & (1 << 25) == 0;
        let has_perf_counter_support = ctrl & (1 << 24) == 0;

        log::info!("DWT info:");
        log::info!(
            " number of comparators available: {}",
            num_comparators_available
        );
        log::info!(" trace sampling support: {}", has_trace_sampling_support);
        log::info!(" compare match support: {}", has_compare_match_support);
        log::info!(" cyccnt support: {}", has_cyccnt_support);
        log::info!(" performance counter support: {}", has_perf_counter_support);
        Ok(())
    }

    pub fn setup_tracing(&mut self) -> Result<(), Error> {
        let mut value = self.component.read_reg(self.core, REG_OFFSET_DWT_CTRL)?;
        value |= 1 << 10; // Sync packet rate.
        value |= 1 << 0; // Enable CYCCNT.
        self.component
            .write_reg(self.core, REG_OFFSET_DWT_CTRL, value)?;
        Ok(())
    }

    /// Enable data monitor on a given user variable at some address
    pub fn enable_trace(&mut self, var_address: u32) -> Result<(), Error> {
        let mask = 0; // size of the ignore mask, ignore nothing!
        let function: u32 = 3; // sample PC and data
                               // function |= 0b10 << 10; // COMP register contains word sized unit.

        // entry 0:
        self.component.write_reg(self.core, 0x20, var_address)?; // COMp value
        self.component.write_reg(self.core, 0x24, mask)?; // mask
        self.component.write_reg(self.core, 0x28, function)?; // function
        Ok(())
    }

    pub fn disable_memory_watch(&mut self) -> Result<(), Error> {
        self.component.write_reg(self.core, 0x28, 0)?; // function, 0 is disabled.
        Ok(())
    }

    pub fn poll(&mut self) -> Result<(), Error> {
        let status = self.component.read_reg(self.core, 0x28)?;
        let matched = status & (1 << 24) > 0;
        log::info!("DWT function0 State: matched={}", matched);
        Ok(())
    }
}
