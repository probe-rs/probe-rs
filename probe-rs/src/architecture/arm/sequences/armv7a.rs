//! Default sequences for ARMv7-A architecture

use crate::{
    architecture::arm::{memory::ArmMemoryInterface, sequences::ArmDebugSequenceError, ArmError},
    MemoryMappedRegister,
};

/// ResetCatchSet for Cortex-A devices
pub fn reset_catch_set(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::Dbgprcr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(core.read_word_32(address)?);

    dbgprcr.set_hcwr(true);

    core.write_word_32(address, dbgprcr.into())?;

    Ok(())
}

/// ResetCatchClear for Cortex-A devices
pub fn reset_catch_clear(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::Dbgprcr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(core.read_word_32(address)?);

    dbgprcr.set_hcwr(false);

    core.write_word_32(address, dbgprcr.into())?;

    Ok(())
}

pub fn reset_system(
    interface: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::{Dbgprcr, Dbgprsr};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    // Request reset
    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(interface.read_word_32(address)?);

    dbgprcr.set_cwrr(true);

    interface.write_word_32(address, dbgprcr.into())?;

    // Wait until reset happens
    let address = Dbgprsr::get_mmio_address_from_base(debug_base)?;

    loop {
        let dbgprsr = Dbgprsr(interface.read_word_32(address)?);
        if dbgprsr.sr() {
            break;
        }
    }

    Ok(())
}

/// DebugCoreStart for v7 Cortex-A devices
pub fn core_start(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::{Dbgdsccr, Dbgdscr, Dbgdsmcr, Dbglar};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;
    tracing::debug!(
        "Starting debug for ARMv7-A core with registers at {:#X}",
        debug_base
    );

    // Lock OS register access to prevent race conditions
    let address = Dbglar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbglar(0).into())?;

    // Force write through / disable caching for debugger access
    let address = Dbgdsccr::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbgdsccr(0).into())?;

    // Disable TLB matching and updates for debugger operations
    let address = Dbgdsmcr::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbgdsmcr(0).into())?;

    // Enable halting
    let address = Dbgdscr::get_mmio_address_from_base(debug_base)?;
    let mut dbgdscr = Dbgdscr(core.read_word_32(address)?);

    if dbgdscr.hdbgen() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }

    dbgdscr.set_hdbgen(true);
    core.write_word_32(address, dbgdscr.into())?;

    Ok(())
}
