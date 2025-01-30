use std::time::{Duration, Instant};

use crate::{
    architecture::arm::{ap::AccessPortError, memory::ArmMemoryInterface, ArmError},
    MemoryMappedRegister,
};

/// DebugCoreStart for Cortex-M devices
pub fn core_start(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Dhcsr;

    let current_dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);

    // Note: Manual addition for debugging, not part of the original DebugCoreStart function
    if current_dhcsr.c_debugen() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }
    // -- End addition

    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.enable_write();

    core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

    Ok(())
}

/// ResetCatchClear for Cortex-M devices
pub fn reset_catch_clear(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Demcr;

    // Clear reset catch bit
    let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
    demcr.set_vc_corereset(false);

    core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
    Ok(())
}

/// ResetCatchSet for Cortex-M devices
pub fn reset_catch_set(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::{Demcr, Dhcsr};

    // Request halt after reset
    let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
    demcr.set_vc_corereset(true);

    core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

    // Clear the status bits by reading from DHCSR
    let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

    Ok(())
}

/// ResetSystem for Cortex-M devices
pub fn reset_system(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::{Aircr, Dhcsr};

    let mut aircr = Aircr(0);
    aircr.vectkey();
    aircr.set_sysresetreq(true);

    interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

    let start = Instant::now();

    while start.elapsed() < Duration::from_millis(500) {
        let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
            Ok(val) => Dhcsr(val),
            // Some combinations of debug probe and target (in
            // particular, hs-probe and ATSAMD21) result in
            // register read errors while the target is
            // resetting.
            Err(ArmError::AccessPort {
                source: AccessPortError::RegisterRead { .. },
                ..
            }) => continue,
            Err(err) => return Err(err),
        };
        if !dhcsr.s_reset_st() {
            return Ok(());
        }
    }

    Err(ArmError::Timeout)
}
