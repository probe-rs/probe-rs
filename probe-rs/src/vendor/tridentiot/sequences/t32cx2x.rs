//! Sequences for the Trident IoT T32Cx2x family (Rafael Micro RT584).

use std::sync::Arc;

use crate::architecture::arm::{
    ArmDebugInterface, ArmError, FullyQualifiedApAddress,
    memory::ArmMemoryInterface,
    sequences::{ArmDebugSequence, cortex_m_core_start},
};
use probe_rs_target::CoreType;

/// The base address of the secure alias of the System Control peripheral.
///
/// System Control defaults to the secure partition out of reset (`sec_peri_ns0[0]` resets to
/// `0`, where `0` means secure), so the secure alias is used here.
const SYSCTRL_BASE_SECURE: u64 = 0x5000_0000;
const SRAM_LOWPOWER_3_OFFSET: u64 = 0x5C;

/// `cfg_peri3_off_sp`: powers off the "Peripherals 3" domain (which includes the Cortex-M33's
/// own debug logic) while the system is in Sleep mode. Defaults to `1` (off).
const CFG_PERI3_OFF_SP: u32 = 1 << 22;

#[derive(Debug)]
pub struct T32Cx2x {}

impl T32Cx2x {
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }
}

impl ArmDebugSequence for T32Cx2x {
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap)?;

        // T32Cx2x only has a single Armv8-M (Cortex-M33) core.
        cortex_m_core_start(&mut *core)?;

        // By default, entering Sleep mode powers off the Cortex-M33's own debug logic, so the
        // debug port becomes unreachable until the chip wakes up on its own (or is hardware
        // reset). Keep that domain powered so an attached session survives a Sleep transition.
        if let Err(error) = keep_debug_logic_powered_in_sleep(&mut *core) {
            tracing::warn!(
                "Failed to keep T32Cx2x debug logic powered in Sleep mode, the debug port may \
                 become unreachable if the target enters Sleep mode: {error}"
            );
        }

        Ok(())
    }
}

fn keep_debug_logic_powered_in_sleep(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let address = SYSCTRL_BASE_SECURE + SRAM_LOWPOWER_3_OFFSET;
    let control = core.read_word_32(address)?;

    if control & CFG_PERI3_OFF_SP != 0 {
        core.write_word_32(address, control & !CFG_PERI3_OFF_SP)?;
    }

    Ok(())
}
