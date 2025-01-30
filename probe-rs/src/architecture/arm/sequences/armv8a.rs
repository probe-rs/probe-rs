//! Default sequences for ARMv7-A architecture

use crate::{
    architecture::arm::{memory::ArmMemoryInterface, sequences::ArmDebugSequenceError, ArmError},
    MemoryMappedRegister,
};

/// ResetCatchSet for ARMv8-A devices
pub fn reset_catch_set(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::Edecr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Edecr::get_mmio_address_from_base(debug_base)?;
    let mut edecr = Edecr(core.read_word_32(address)?);

    edecr.set_rce(true);

    core.write_word_32(address, edecr.into())?;

    Ok(())
}

/// ResetCatchClear for ARMv8-a devices
pub fn reset_catch_clear(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::Edecr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Edecr::get_mmio_address_from_base(debug_base)?;
    let mut edecr = Edecr(core.read_word_32(address)?);

    edecr.set_rce(false);

    core.write_word_32(address, edecr.into())?;

    Ok(())
}

pub fn reset_system(
    interface: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::{Edprcr, Edprsr};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    // Request reset
    let address = Edprcr::get_mmio_address_from_base(debug_base)?;
    let mut edprcr = Edprcr(interface.read_word_32(address)?);

    edprcr.set_cwrr(true);

    interface.write_word_32(address, edprcr.into())?;

    // Wait until reset happens
    let address = Edprsr::get_mmio_address_from_base(debug_base)?;

    loop {
        let edprsr = Edprsr(interface.read_word_32(address)?);
        if edprsr.sr() {
            break;
        }
    }

    Ok(())
}

/// DebugCoreStart for v8 Cortex-A devices
pub fn core_start(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
    cti_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::{
        CtiControl, CtiGate, CtiOuten, Edlar, Edscr, Oslar,
    };

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;
    let cti_base =
        cti_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::CtiBaseNotSpecified))?;

    tracing::debug!(
        "Starting debug for ARMv8-A core with registers at {:#X}",
        debug_base
    );

    // Lock OS register access to prevent race conditions
    let address = Edlar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Edlar(0).into())?;

    // Unlock the OS Lock to enable access to debug registers
    let address = Oslar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Oslar(0).into())?;

    // Configure CTI
    let mut cticontrol = CtiControl(0);
    cticontrol.set_glben(true);

    let address = CtiControl::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, cticontrol.into())?;

    // Gate all events by default
    let address = CtiGate::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, 0)?;

    // Configure output channels for halt and resume
    // Channel 0 - halt requests
    let mut ctiouten = CtiOuten(0);
    ctiouten.set_outen(0, 1);

    let address = CtiOuten::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, ctiouten.into())?;

    // Channel 1 - resume requests
    let mut ctiouten = CtiOuten(0);
    ctiouten.set_outen(1, 1);

    let address = CtiOuten::get_mmio_address_from_base(cti_base)? + 4;
    core.write_word_32(address, ctiouten.into())?;

    // Enable halting
    let address = Edscr::get_mmio_address_from_base(debug_base)?;
    let mut edscr = Edscr(core.read_word_32(address)?);

    if edscr.hde() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }

    edscr.set_hde(true);
    core.write_word_32(address, edscr.into())?;

    Ok(())
}
