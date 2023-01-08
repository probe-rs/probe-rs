//! Sequences for Nrf52 devices

use std::sync::Arc;

use super::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::{
    component::TraceSink, memory::CoresightComponent, ArmError, ArmProbeInterface,
};

/// An error when operating a core ROM table component occurred.
#[derive(thiserror::Error, Debug)]
pub enum ComponentError {
    /// Nordic chips do not support setting all TPIU clocks. Try choosing another clock speed.
    #[error("Nordic does not support TPIU CLK value of {0}")]
    NordicUnsupportedTPUICLKValue(u32),

    /// Nordic chips do not have an embedded trace buffer.
    #[error("nRF52 devices do not have a trace buffer")]
    NordicNoTraceMem,
}

/// Marker struct indicating initialization sequencing for nRF52 family parts.
pub struct Nrf52 {}

impl Nrf52 {
    /// Create the sequencer for the nRF52 family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

mod clock {
    use crate::architecture::arm::{memory::adi_v5_memory_interface::ArmProbe, ArmError};
    use bitfield::bitfield;

    /// The base address of the DBGMCU component
    const CLOCK: u64 = 0x4000_0000;

    bitfield! {
        /// The TRACECONFIG register of the CLOCK peripheral. This register is described in
        /// "nRF52840 Product Specification" section 5.4.3.11
        pub struct TraceConfig(u32);
        impl Debug;

        pub u8, traceportspeed, set_traceportspeed: 1, 0;

        pub u8, tracemux, set_tracemux: 17, 16;
    }

    impl TraceConfig {
        /// The offset of the Control register in the DBGMCU block.
        const ADDRESS: u64 = 0x55C;

        /// Read the control register from memory.
        pub fn read(memory: &mut dyn ArmProbe) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(CLOCK + Self::ADDRESS)?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub fn write(&mut self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
            memory.write_word_32(CLOCK + Self::ADDRESS, self.0)
        }
    }
}

impl ArmDebugSequence for Nrf52 {
    fn trace_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        components: &[CoresightComponent],
        sink: &TraceSink,
    ) -> Result<(), ArmError> {
        let tpiu_clock = match sink {
            TraceSink::TraceMemory => {
                tracing::error!("nRF52 does not have a trace buffer");
                return Err(ArmError::from(ComponentError::NordicNoTraceMem));
            }

            TraceSink::Tpiu(config) => config.tpiu_clk(),
            TraceSink::Swo(config) => config.tpiu_clk(),
        };

        let portspeed = match tpiu_clock {
            4_000_000 => 3,
            8_000_000 => 2,
            16_000_000 => 1,
            32_000_000 => 0,
            tpiu_clk => {
                let e = ComponentError::NordicUnsupportedTPUICLKValue(tpiu_clk);
                tracing::error!("{:?}", e);
                return Err(ArmError::from(e));
            }
        };

        let mut memory = interface.memory_interface(components[0].ap)?;
        let mut config = clock::TraceConfig::read(&mut *memory)?;
        config.set_traceportspeed(portspeed);
        if matches!(sink, TraceSink::Tpiu(_)) {
            config.set_tracemux(2);
        } else {
            config.set_tracemux(1);
        }

        config.write(&mut *memory)?;

        Ok(())
    }
}

impl From<ComponentError> for ArmError {
    fn from(value: ComponentError) -> ArmError {
        ArmError::DebugSequence(ArmDebugSequenceError::custom(value))
    }
}
