//! Sequences for STM32N6 devices

use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        ap::{ApRegister, DRW, TAR},
        armv8m::Aircr,
        core::{
            cortex_m::write_core_reg,
            registers::{
                armv8m::V8M_MAIN_SEC_FP_REGISTERS, cortex_m::CORTEX_M_WITH_FP_CORE_REGISTERS,
            },
        },
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, cortex_m_wait_for_reset},
    },
};

/// The DBGMCU AP can be used to monitor the state of the
/// Cortex-M55 AP
const DBGMCU_AP: FullyQualifiedApAddress = FullyQualifiedApAddress::v1_with_default_dp(0);
const DBGMCU_SR: u32 = 0x8000_10fc;

/// Marker struct indicating initialization sequencing for STM32N6 family parts.
#[derive(Debug)]
pub struct Stm32n6 {}

impl Stm32n6 {
    /// Create the sequencer for the STM32N6 family of parts.
    pub fn create() -> Arc<Self> {
        Arc::new(Self {})
    }

    fn read_dbgmcu(
        &self,
        arm_interface: &mut dyn ArmDebugInterface,
        address: u32,
    ) -> Result<u32, ArmError> {
        arm_interface.write_raw_ap_register(&DBGMCU_AP, TAR::ADDRESS, address)?;
        arm_interface.read_raw_ap_register(&DBGMCU_AP, DRW::ADDRESS)
    }
}

impl ArmDebugSequence for Stm32n6 {
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let ap = interface.fully_qualified_address();

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        interface.flush()?;

        let start = Instant::now();

        let arm_interface = interface.get_arm_debug_interface()?;

        // After reset, AP1 is locked until the boot ROM opens it.
        //
        // Poll DBGMCU_SR.AP1_ENABLE (addr 0x8000_10FC bit 17) on AP0
        // to see when it unlocks.
        loop {
            if start.elapsed() > Duration::from_millis(500) {
                tracing::warn!("reset timed out after {:?} ms", start.elapsed());
                return Err(ArmError::Timeout);
            }
            let dbgmcu_sr = self.read_dbgmcu(arm_interface, DBGMCU_SR)?;
            tracing::trace!("DBGMCU_SR: {dbgmcu_sr:08x}");
            if dbgmcu_sr & 1 << 17 != 0 {
                tracing::info!(
                    "AP1 is out of reset after {} msec",
                    start.elapsed().as_millis()
                );
                break;
            }
        }

        tracing::trace!("Waiting for core to reset...");
        cortex_m_wait_for_reset(interface).ok();

        // Restart the debug core interface
        tracing::trace!("Restarting debug core");

        // Now wait for the main core on AP1 to successfully start. AP1 is closed
        // and unavailable until the boot rom enables it, so keep trying until
        // it becomes available.
        let arm_interface = interface.get_arm_debug_interface()?;
        let start = Instant::now();
        loop {
            if self
                .debug_core_start(arm_interface, &ap, core_type, debug_base, None)
                .is_ok()
            {
                break;
            }
            // If the core failed to start, reinitialize the entire connection
            arm_interface.reinitialize().ok();

            if start.elapsed() > Duration::from_millis(500) {
                tracing::error!("Debug core didn't start");
                return Err(ArmError::Timeout);
            }
        }
        tracing::trace!("Core restarted after {} ms", start.elapsed().as_millis());

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Try to restore the CPU to a known state after halting. This is necessary on the STM32N6
        // where we can't `reset_and_halt()` into a known state because the boot ROM always runs.
        // See TakeReset() in the arch manual, and DCRSR.REGSEL for register indices.
        for reg in V8M_MAIN_SEC_FP_REGISTERS.all_registers() {
            write_core_reg(core, reg.into(), 0u32)?;
        }

        // Set thumb bit in PSR; otherwise we'll immediately fault.
        write_core_reg(
            core,
            CORTEX_M_WITH_FP_CORE_REGISTERS.psr().unwrap().into(),
            0x0100_0000_u32,
        )?;

        // Mask interrupts by writing to EXTRA since the NVIC state is unknown.
        write_core_reg(
            core,
            CORTEX_M_WITH_FP_CORE_REGISTERS
                .other_by_name("EXTRA")
                .unwrap()
                .into(),
            0x0000_0001_u32,
        )?;

        // Set FPCCR.ASPEN and FPCCR.LSPEN to their default reset values
        // according to TakeReset().
        core.write_word_32(0xE000_EF34, 0xc000_0000)?;

        // Disable caches by clearing bits in SCB_CCR.
        core.write_word_32(0xE000_ED14, 0x201)?;

        // Clear instruction cache by writing to ICIALLU.
        core.write_word_32(0xE000_EF50, 0)?;

        // If the reset is being caught, then it might be the case that the user will want
        // to load software into SRAM. Enable the SRAM blocks to allow for this.

        // Enable AXISRAM3,4,5,6 which are all disabled after reset
        const RCC_MEMENR: u64 = 0x4602_824C;
        const RCC_MEMENR_AXISRAM3456_EN: u32 = 0xF;
        core.read_word_32(RCC_MEMENR)
            .and_then(|val| core.write_word_32(RCC_MEMENR, val | RCC_MEMENR_AXISRAM3456_EN))
            .inspect_err(|_| tracing::error!("failed to enable axisram"))?;

        // AXISRAM 3..6 are powered down by default; clear the SRAMSD bit.
        for sram in 3..=6 {
            const RAMCFG_BASE: u64 = 0x42023000;
            const RAMCFG_AXISRAMXCR_BASE: u64 = RAMCFG_BASE;
            let ramcfg_axisramncr = RAMCFG_AXISRAMXCR_BASE + 0x80 * (sram - 1);
            core.write_word_32(ramcfg_axisramncr, 0)
                .inspect_err(|_| tracing::error!("failed to power up AXISRAM"))?;
        }

        Ok(())
    }
}
