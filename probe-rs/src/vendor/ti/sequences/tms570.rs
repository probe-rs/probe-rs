//! Sequences for tms570 devices
use std::sync::Arc;

use crate::architecture::arm::communication_interface::DapProbe;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::{ArmError, dp::DpAddress};
use crate::probe::WireProtocol;

use super::icepick::Icepick;

/// Marker struct indicating initialization sequencing for cc13xx_cc26xx family parts.
#[derive(Debug)]
pub struct TMS570 {}

impl TMS570 {
    /// Create the sequencer for the cc13xx_cc26xx family of parts.
    pub fn create(_name: String) -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl ArmDebugSequence for TMS570 {
    fn reset_hardware_assert(&self, _interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        Ok(())
    }

    fn reset_hardware_deassert(
        &self,
        _probe: &mut dyn crate::architecture::arm::ArmProbeInterface,
        _default_ap: &crate::architecture::arm::FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        Ok(())
    }

    fn reset_system(
        &self,
        _probe: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        Ok(())
    }

    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        _dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Ensure current debug interface is in reset state.
        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF)?;

        match interface.active_protocol() {
            Some(WireProtocol::Jtag) => {
                let mut icepick = Icepick::new(interface)?;
                icepick.select_tap(0)?;

                // Call the configure JTAG function. We don't derive the scan chain at runtime
                // for these devices, but regardless the scan chain must be told to the debug probe
                // We avoid the live scan for the following reasons:
                // 1. Only the ICEPICK is connected at boot so we need to manually the CPU to the scan chain
                // 2. Entering test logic reset disconects the CPU again
                interface.configure_jtag(true)?;
            }
            Some(WireProtocol::Swd) => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "The tms570 family doesn't support SWD".into(),
                )
                .into());
            }
            _ => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        Ok(())
    }
}
