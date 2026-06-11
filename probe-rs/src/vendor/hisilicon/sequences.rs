//! Chip-specific debug bring-up for HiSilicon RISC-V SoCs.
//!
//! WS63 (Hi3863) is a HiSilicon "riscv31" RISC-V core whose Debug Module is
//! reached through an ARM CoreSight DAP (AHB-AP 0, DM @ `0x8000_0000`) — the same
//! `ArmWithRiscv` topology probe-rs uses for RP2350. The RISC-V debug interface
//! itself is brought up by the generic mem-AP DTM (see
//! [`RiscvCoreAccessOptions::dm_base`]); the only chip-specific step is at the
//! ARM-DAP level, so this is an [`ArmDebugSequence`]. A `RiscvDebugSequence` hook
//! would run too late — `on_connect` fires only after the DM has already been
//! accessed by `enter_debug_mode`.
//!
//! Unvalidated on silicon. Register addresses are reverse-engineered from HiSpark
//! Studio's patched OpenOCD (`tcl/target/vendorhm/WS63-*.cfg`).
//!
//! [`RiscvCoreAccessOptions::dm_base`]: probe_rs_target::RiscvCoreAccessOptions

use std::sync::Arc;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress, sequences::ArmDebugSequence,
};

/// WS63 control register that routes the debug pads to the CoreSight DAP.
///
/// From the WS63 OpenOCD target cfg connect note: `# enable coresight-swd mode`
/// / `mww 0x40010260 1`.
const WS63_CORESIGHT_ENABLE: u64 = 0x4001_0260;

/// Debug sequence for the HiSilicon WS63 (Hi3863).
#[derive(Debug)]
pub struct Ws63;

impl Ws63 {
    /// Create a WS63 debug sequence.
    pub fn create() -> Arc<Self> {
        Arc::new(Ws63)
    }
}

impl ArmDebugSequence for Ws63 {
    /// Route the WS63 debug pads to the CoreSight DAP ("enable coresight-swd
    /// mode") so the RISC-V Debug Module behind AP0 becomes reachable.
    ///
    /// Best-effort: on most boards the debug port is enabled by the external
    /// strap (GPIO_04 high at power-on, per the WS63 hardware guide), which
    /// probe-rs cannot perform. If the register write fails we log and continue
    /// rather than abort attach, since the strap may already have enabled it.
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let mut memory = interface.memory_interface(default_ap)?;
        match memory.write_word_32(WS63_CORESIGHT_ENABLE, 1) {
            Ok(()) => {
                let _ = memory.flush();
                tracing::debug!(
                    "WS63: enabled CoreSight-SWD debug path (0x{WS63_CORESIGHT_ENABLE:08x} = 1)"
                );
            }
            Err(e) => tracing::warn!(
                "WS63: CoreSight-SWD enable write failed ({e:?}); continuing — the debug \
                 pads are normally enabled by the external GPIO_04 power-on strap"
            ),
        }
        Ok(())
    }
}
