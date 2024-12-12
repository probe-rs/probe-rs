//! Module for using the ITM.
//!
//! ITM = Instrumentation Trace Macrocell

use super::super::memory::romtable::CoresightComponent;
use super::DebugComponentInterface;
use crate::architecture::arm::ArmProbeInterface;
use crate::{Error, MemoryMappedRegister};

pub const _ITM_PID: [u8; 8] = [0x1, 0xB0, 0x3b, 0x0, 0x4, 0x0, 0x0, 0x0];

/// An interface to control the ITM (Instrumentation Trace Macrocell) of a MCU.
///
/// The ITM generates trace information as packets. There are four sources that can generate packets.
/// If multiple sources generate packets at the same time, the ITM arbitrates the order in which packets are output.
///
/// All the information is single direction Target -> Host.
///
/// The four sources in decreasing order of priority are:
/// - Software trace. Software can write directly to ITM stimulus registers to generate packets.
/// - Hardware trace. The DWT generates these packets, and the ITM outputs them.
/// - Time stamping. Timestamps are generated relative to packets.
///   The ITM contains a 21-bit counter to generate the timestamp.
///   The Cortex-M4 clock or the bitclock rate of the Serial Wire Viewer (SWV) output clocks the counter.
/// - Global system timestamping. Timestamps can optionally be generated using a system-wide 48-bit count value.
///   The same count value can be used to insert timestamps in the ETM trace stream, allowing coarse-grain correlation.
pub struct Itm<'a> {
    component: &'a CoresightComponent,
    interface: &'a mut dyn ArmProbeInterface,
}

const _REGISTER_OFFSET_ITM_TPR: u32 = 0xE40;
const REGISTER_OFFSET_ACCESS: u32 = 0xFB0;

impl<'a> Itm<'a> {
    /// Create a new ITM interface from a probe and a ROM table component.
    pub fn new(
        interface: &'a mut dyn ArmProbeInterface,
        component: &'a CoresightComponent,
    ) -> Self {
        Itm {
            interface,
            component,
        }
    }

    /// Unlock the ITM and enable it for tracing the target.
    ///
    /// This function enables the ITM unit as a whole. It does not actually send any data after enabling it.
    ///
    /// To enable actual transaction of data, see [`Itm::tx_enable`].
    pub fn unlock(&mut self) -> Result<(), Error> {
        self.component
            .write_reg(self.interface, REGISTER_OFFSET_ACCESS, 0xC5AC_CE55)?;

        Ok(())
    }

    /// Enable the ITM TX to send tracing data to the TPIU.
    ///
    /// This enables the actual TX pin of the overarching TPIU which is the parent peripheral of the ITM that multiplexes all data.
    pub fn tx_enable(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;

        tcr.set_itmena(true); // ITMENA: enable ITM (master switch)
        tcr.set_tsena(true); // TSENA: enable local timestamps
        tcr.set_syncena(true); // SYNENA: Enable sync pulses, note DWT_CTRL.SYNCTAP must be configured.
        tcr.set_txena(true); // TXENA: forward DWT packets to ITM
        tcr.set_gtsfreq(0b10); // GTSFREQ: generate global timestamp every 8192 cycles
        tcr.set_trace_bus_id(0b1101); // 7 bits trace bus ID

        tcr.store(self.component, self.interface)?;

        // Enable all 32 channels.
        self.component.write_reg(
            self.interface,
            register::ITM_TER::ADDRESS_OFFSET as u32,
            register::ITM_TER::enable_all().into(),
        )?;

        Ok(())
    }

    /// Enable all 32 stimulus registers.
    pub fn enable_stim_all(&mut self) -> Result<(), Error> {
        self.component.write_reg(
            self.interface,
            register::ITM_TER::ADDRESS_OFFSET as u32,
            register::ITM_TER::enable_all().into(),
        )?;

        Ok(())
    }

    /// Disable all 32 stimulus registers.
    pub fn disable_stim_all(&mut self) -> Result<(), Error> {
        self.component.write_reg(
            self.interface,
            register::ITM_TER::ADDRESS_OFFSET as u32,
            register::ITM_TER::disable_all().into(),
        )?;

        Ok(())
    }

    /// Enable a stimulus register.
    pub fn enable_stim(&mut self, port: u8) -> Result<(), Error> {
        let mut value = self
            .component
            .read_reg(self.interface, register::ITM_TER::ADDRESS_OFFSET as u32)?;
        value |= 1 << port;
        self.component.write_reg(
            self.interface,
            register::ITM_TER::ADDRESS_OFFSET as u32,
            value,
        )?;
        Ok(())
    }

    /// Disable a stimulus register.
    pub fn disable_stim(&mut self, port: u8) -> Result<(), Error> {
        let mut value = self
            .component
            .read_reg(self.interface, register::ITM_TER::ADDRESS_OFFSET as u32)?;
        value &= !(1 << port);
        self.component.write_reg(
            self.interface,
            register::ITM_TER::ADDRESS_OFFSET as u32,
            value,
        )?;
        Ok(())
    }

    /// Enable local timestamps generation.
    pub fn enable_local_timestamps(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_tsena(true);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }

    /// Disable local timestamps generation.
    pub fn disable_local_timestamps(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_tsena(false);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }

    /// Enable synchronization packet transmission.  
    pub fn enable_sync_pulses(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_syncena(true);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }

    /// Disable synchronization packet transmission.  
    pub fn disable_sync_pulses(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_syncena(false);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }

    /// Enable forwarding DWT packets to the ITM.
    pub fn enable_forward_dwt(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_txena(true);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }

    /// Disable forwarding DWT packets to the ITM.
    pub fn disable_forward_dwt(&mut self) -> Result<(), Error> {
        let mut tcr = register::ITM_TCR::load(self.component, self.interface)?;
        tcr.set_txena(false);
        tcr.store(self.component, self.interface)?;

        Ok(())
    }
}

mod register {
    use crate::{
        architecture::arm::component::DebugComponentInterface, memory_mapped_bitfield_register,
    };

    memory_mapped_bitfield_register! {
        pub struct ITM_TER(u32);
        0xE00,"ITM_TER",
        impl From;

        pub stim31, set_stim31: 31;
        pub stim30, set_stim30: 30;
        pub stim29, set_stim29: 29;
        pub stim28, set_stim28: 28;
        pub stim27, set_stim27: 27;
        pub stim26, set_stim26: 26;
        pub stim25, set_stim25: 25;
        pub stim24, set_stim24: 24;
        pub stim23, set_stim23: 23;
        pub stim22, set_stim22: 22;
        pub stim21, set_stim21: 21;
        pub stim20, set_stim20: 20;
        pub stim19, set_stim19: 19;
        pub stim18, set_stim18: 18;
        pub stim17, set_stim17: 17;
        pub stim16, set_stim16: 16;
        pub stim15, set_stim15: 15;
        pub stim14, set_stim14: 14;
        pub stim13, set_stim13: 13;
        pub stim12, set_stim12: 12;
        pub stim11, set_stim11: 11;
        pub stim10, set_stim10: 10;
        pub stim09, set_stim09: 9;
        pub stim08, set_stim08: 8;
        pub stim07, set_stim07: 7;
        pub stim06, set_stim06: 6;
        pub stim05, set_stim05: 5;
        pub stim04, set_stim04: 4;
        pub stim03, set_stim03: 3;
        pub stim02, set_stim02: 2;
        pub stim01, set_stim01: 1;
        pub stim00, set_stim00: 0;
    }

    impl ITM_TER {
        pub fn enable_all() -> Self {
            Self(0xFFFF_FFFF)
        }

        pub fn disable_all() -> Self {
            Self(0x0000_0000)
        }
    }

    memory_mapped_bitfield_register! {
        pub struct ITM_TCR(u32);
        0xE80, "ITM_TCR",
        impl From;

        pub busy, set_busy: 23;
        pub u8, tracebusid, set_trace_bus_id: 22, 16;
        pub u8, gtsfreq, set_gtsfreq: 11, 10;
        pub u8, tsprescale, set_tsprescale: 9, 8;
        pub swoena, set_swoena: 4;
        pub txena, set_txena: 3;
        pub syncena, set_syncena: 2;
        pub tsena, set_tsena: 1;
        pub itmena, set_itmena: 0;
    }

    impl DebugComponentInterface for ITM_TCR {}
}
