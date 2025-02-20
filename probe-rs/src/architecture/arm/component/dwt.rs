//! Interface with the DWT (data watchpoint and trace) unit.
//!
//! This unit can monitor specific memory locations for write / read
//! access, this could be handy to debug a system :).
//!
//! See ARMv7-M architecture reference manual C1.8 for some additional
//! info about this stuff.

use super::super::memory::romtable::CoresightComponent;
use super::DebugComponentInterface;
use crate::architecture::arm::{ArmError, ArmProbeInterface};
use crate::{Error, memory_mapped_bitfield_register};

/// A struct representing a DWT unit on target.
pub struct Dwt<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

impl<'a> Dwt<'a> {
    /// Creates a new DWT component representation.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Dwt {
            interface,
            component,
        }
    }

    /// Logs some info about the DWT component.
    pub fn info(&mut self) -> Result<(), Error> {
        let ctrl = Ctrl::load(self.component, self.interface)?;

        tracing::info!("DWT info:");
        tracing::info!("  number of comparators available: {}", ctrl.numcomp());
        tracing::info!("  trace sampling support: {}", !ctrl.notrcpkt());
        tracing::info!("  compare match support: {}", !ctrl.noexttrig());
        tracing::info!("  cyccnt support: {}", !ctrl.nocyccnt());
        tracing::info!("  performance counter support: {}", !ctrl.noprfcnt());

        Ok(())
    }

    /// Enables the DWT component.
    pub fn enable(&mut self) -> Result<(), ArmError> {
        let mut ctrl = Ctrl::load(self.component, self.interface)?;
        ctrl.set_synctap(0x01);
        ctrl.set_cyccntena(true);
        ctrl.store(self.component, self.interface)
    }

    /// Enables data tracing on a specific address in memory on a specific DWT unit.
    pub fn enable_data_trace(&mut self, unit: usize, address: u32) -> Result<(), ArmError> {
        let mut comp = Comp::load_unit(self.component, self.interface, unit)?;
        comp.set_comp(address);
        comp.store_unit(self.component, self.interface, unit)?;

        let mut mask = Mask::load_unit(self.component, self.interface, unit)?;
        mask.set_mask(0x0);
        mask.store_unit(self.component, self.interface, unit)?;

        let mut function = Function::load_unit(self.component, self.interface, unit)?;
        function.set_datavsize(0x10);
        function.set_emitrange(false);
        function.set_datavmatch(false);
        function.set_cycmatch(false);
        function.set_function(0b11);

        function.store_unit(self.component, self.interface, unit)
    }

    /// Disables data tracing on the given unit.
    pub fn disable_data_trace(&mut self, unit: usize) -> Result<(), ArmError> {
        let mut function = Function::load_unit(self.component, self.interface, unit)?;
        function.set_function(0x0);
        function.store_unit(self.component, self.interface, unit)
    }

    /// Enable exception tracing.
    pub fn enable_exception_trace(&mut self) -> Result<(), ArmError> {
        let mut ctrl = Ctrl::load(self.component, self.interface)?;
        ctrl.set_exctrcena(true);
        ctrl.store(self.component, self.interface)
    }

    /// Disable exception tracing.
    pub fn disable_exception_trace(&mut self) -> Result<(), ArmError> {
        let mut ctrl = Ctrl::load(self.component, self.interface)?;
        ctrl.set_exctrcena(false);
        ctrl.store(self.component, self.interface)
    }

    /// Enable PC sample trace output
    pub fn enable_pc_sampling(&mut self) -> Result<(), ArmError> {
        let mut ctrl = Ctrl::load(self.component, self.interface)?;
        ctrl.set_pcsamplena(true);
        ctrl.set_cyctap(true);
        ctrl.set_postpreset(0x3);
        ctrl.store(self.component, self.interface)
    }

    /// Disable PC sample trace output
    pub fn disable_pc_sampling(&mut self) -> Result<(), ArmError> {
        let mut ctrl = Ctrl::load(self.component, self.interface)?;
        ctrl.set_pcsamplena(false);
        ctrl.set_cyctap(false);
        ctrl.set_postpreset(0x0);
        ctrl.store(self.component, self.interface)
    }

    /// Read the program counter sample register for the PC value.
    ///
    /// This is an optional DWT component, so your DWT may not implement
    /// it. An implementation that doesn't include this component returns
    /// zero.
    ///
    /// Make sure that tracing is enabled. Otherwise, the value is unknown.
    ///
    /// The PC value is `!0` if the processor is in a debug state or another
    /// state that disables the DWT.
    ///
    /// For more information, see section C1.8.5 of the ARMv7-M architecture
    /// reference manual.
    pub fn read_pcsr(&mut self) -> Result<u32, ArmError> {
        Ok(Pcsr::load(self.component, self.interface)?.eiasample())
    }
}

memory_mapped_bitfield_register! {
    pub struct Ctrl(u32);
    0x00, "DWT/CTRL",
    impl From;
    pub u8, numcomp, _: 31, 28;
    pub notrcpkt, _: 27;
    pub noexttrig, _: 26;
    pub nocyccnt, _: 25;
    pub noprfcnt, _: 24;
    pub cycevtena, set_cycevtena: 22;
    pub foldevtena, set_foldevtena: 21;
    pub lsuevtena, set_lsuevtena: 20;
    pub sleepevtena, set_sleepevtena: 19;
    pub excevtena, set_excevtena: 18;
    pub cpievtena, set_cpievtena: 17;
    pub exctrcena, set_exctrcena: 16;
    pub pcsamplena, set_pcsamplena: 12;
    /// 00 Disabled. No Synchronization packets.
    /// 01 Synchronization counter tap at CYCCNT[24].
    /// 10 Synchronization counter tap at CYCCNT[26].
    /// 11 Synchronization counter tap at CYCCNT[28].
    pub u8, synctap, set_synctap: 11, 10;
    pub cyctap, set_cyctap: 9;
    pub u8, postinit, set_postinit: 8, 5;
    pub postpreset, set_postpreset: 4, 1;
    pub cyccntena, set_cyccntena: 0;

}

impl DebugComponentInterface for Ctrl {}

memory_mapped_bitfield_register! {
    pub struct Cyccnt(u32);
    0x04, "DWT/CYCCNT",
    impl From;
}

memory_mapped_bitfield_register! {
    pub struct Cpicnt(u32);
    0x08, "DWT/CPICNT",
    impl From;
}

memory_mapped_bitfield_register! {
    pub struct Exccnt(u32);
    0x0C, "DWT/EXCCNT",
    impl From;
}

memory_mapped_bitfield_register! {
    pub struct Comp(u32);
    0x20, "DWT/COMP",
    impl From;
    pub u32, comp, set_comp: 31, 0;
}

impl DebugComponentInterface for Comp {}

memory_mapped_bitfield_register! {
    pub struct Mask(u32);
    0x24, "DWT/MASK",
    impl From;
    pub u32, mask, set_mask: 4, 0;
}

impl DebugComponentInterface for Mask {}

memory_mapped_bitfield_register! {
    pub struct Function(u32);
    0x28, "DWT/FUNCTION",
    impl From;
    pub matched, _: 24;
    pub u8, datavaddr1, set_datavaddr1: 19, 16;
    pub u8, datavaddr0, set_datavaddr0: 15, 12;
    /// 00 Byte.
    /// 01 Halfword.
    /// 10 Word.
    pub u8, datavsize, set_datavsize: 11, 10;
    pub lnk1ena, _: 9;
    pub datavmatch, set_datavmatch: 8;
    pub cycmatch, set_cycmatch: 7;
    pub emitrange, set_emitrange: 5;
    pub function, set_function: 3, 0;
}

impl DebugComponentInterface for Function {}

memory_mapped_bitfield_register! {
    pub struct Pcsr(u32);
    0x1C, "DWT/PCSR",
    impl From;
    pub u32, eiasample, _: 31, 0;
}

impl DebugComponentInterface for Pcsr {}
