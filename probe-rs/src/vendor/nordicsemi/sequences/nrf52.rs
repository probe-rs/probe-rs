//! Sequences for Nrf52 devices

use std::sync::Arc;

use crate::architecture::arm::{
    ArmError, ArmProbeInterface, FullyQualifiedApAddress,
    component::TraceSink,
    memory::CoresightComponent,
    sequences::{ArmDebugSequence, ArmDebugSequenceError},
};
use crate::session::MissingPermissions;

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

const RESET: u64 = 0x00;
const ERASEALL: u64 = 0x04;
const ERASEALLSTATUS: u64 = 0x08;
const APPROTECTSTATUS: u64 = 0x0C;

/// Marker struct indicating initialization sequencing for nRF52 family parts.
#[derive(Debug)]
pub struct Nrf52 {}

impl Nrf52 {
    /// Create the sequencer for the nRF52 family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    async fn is_core_unlocked(
        &self,
        iface: &mut dyn ArmProbeInterface,
        ctrl_ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let status = iface.read_raw_ap_register(ctrl_ap, APPROTECTSTATUS).await?;
        Ok(status != 0)
    }
}

mod clock {
    use crate::architecture::arm::{ArmError, memory::ArmMemoryInterface};
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
        pub async fn read(memory: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
            let contents = memory.read_word_32(CLOCK + Self::ADDRESS).await?;
            Ok(Self(contents))
        }

        /// Write the control register to memory.
        pub async fn write(&mut self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
            memory.write_word_32(CLOCK + Self::ADDRESS, self.0).await
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for Nrf52 {
    async fn debug_device_unlock(
        &self,
        iface: &mut dyn ArmProbeInterface,
        _default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let ctrl_ap = &FullyQualifiedApAddress::v1_with_default_dp(1);

        tracing::info!("Checking if core is unlocked");
        if self.is_core_unlocked(iface, ctrl_ap).await? {
            tracing::info!("Core is already unlocked");
            return Ok(());
        }

        tracing::warn!("Core is locked. Erase procedure will be started to unlock it.");
        permissions
            .erase_all()
            .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

        // Reset
        iface.write_raw_ap_register(ctrl_ap, RESET, 1).await?;
        iface.write_raw_ap_register(ctrl_ap, RESET, 0).await?;

        // Start erase
        iface.write_raw_ap_register(ctrl_ap, ERASEALL, 1).await?;

        // Wait for erase done
        while iface.read_raw_ap_register(ctrl_ap, ERASEALLSTATUS).await? != 0 {}

        // Reset again
        iface.write_raw_ap_register(ctrl_ap, RESET, 1).await?;
        iface.write_raw_ap_register(ctrl_ap, RESET, 0).await?;

        if !self.is_core_unlocked(iface, ctrl_ap).await? {
            return Err(ArmDebugSequenceError::custom("Could not unlock core").into());
        }

        Err(ArmError::ReAttachRequired)
    }

    async fn trace_start(
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

        let mut memory = interface
            .memory_interface(&components[0].ap_address)
            .await?;
        let mut config = clock::TraceConfig::read(&mut *memory).await?;
        config.set_traceportspeed(portspeed);
        if matches!(sink, TraceSink::Tpiu(_)) {
            config.set_tracemux(2);
        } else {
            config.set_tracemux(1);
        }

        config.write(&mut *memory).await?;

        Ok(())
    }
}

impl From<ComponentError> for ArmError {
    fn from(value: ComponentError) -> ArmError {
        ArmError::DebugSequence(ArmDebugSequenceError::custom(value))
    }
}
