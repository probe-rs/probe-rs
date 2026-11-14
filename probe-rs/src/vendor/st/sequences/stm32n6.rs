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
        armv8m::{Aircr, Dhcsr},
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

        let dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);
        if !dhcsr.s_sde() || dhcsr.s_suide() || dhcsr.s_nsuide() {
            return Err(ArmError::Other(
                "STM32N6 reset cleanup requires privileged Secure and Non-secure debug access"
                    .into(),
            ));
        }

        // DAP accesses to the Non-secure SCS aliases are RAZ/WI. Select the bank with
        // DSCSR instead (Armv8-M ARM B13.3, D1.2.51). CDSKEY=1 prevents changing CDS.
        const DSCSR: u64 = 0xE000_EE08;
        const DSCSR_CDSKEY: u32 = 1 << 17;
        const DSCSR_SELECTION: u32 = 3;
        let previous_dscsr = core.read_word_32(DSCSR)? | DSCSR_CDSKEY;
        let result = (|| {
            // The core remains halted while disabling both SysTicks and clearing the banked
            // SysTick/PendSV pending bits, before either PRIMASK bank is cleared below.
            // SBRSELEN=1, with SBRSEL=0 (Non-secure) then SBRSEL=1 (Secure).
            for selection in [1, 3] {
                let dscsr = (previous_dscsr & !DSCSR_SELECTION) | selection;
                core.write_word_32(DSCSR, dscsr)?;
                if core.read_word_32(DSCSR)? & DSCSR_SELECTION != selection {
                    return Err(ArmError::Other(
                        "STM32N6 SCS bank selection did not take effect".into(),
                    ));
                }
                const SYST_CSR: u64 = 0xE000_E010;
                const SCB_ICSR: u64 = 0xE000_ED04;
                core.write_word_32(SYST_CSR, 0)?;
                core.write_word_32(SCB_ICSR, (1 << 25) | (1 << 27))?;
            }

            // NVIC enable/pending bits are shared. Leave the Secure view selected so that
            // interrupts targeted to either security state are accessible.
            const ICTR: u64 = 0xE000_E004;
            const NVIC_ICER: u64 = 0xE000_E180;
            const NVIC_ICPR: u64 = 0xE000_E280;
            let interrupt_registers = (core.read_word_32(ICTR)? & 0xf) + 1;
            for index in 0..interrupt_registers {
                let offset = u64::from(index) * 4;
                core.write_word_32(NVIC_ICER + offset, u32::MAX)?;
                core.write_word_32(NVIC_ICPR + offset, u32::MAX)?;
            }
            Ok(())
        })();

        // Even a failed selection write may have reached the core. Always attempt to restore
        // the previous selection, and preserve the cleanup error if restoration also fails.
        let restore = (|| {
            core.write_word_32(DSCSR, previous_dscsr)?;
            if core.read_word_32(DSCSR)? & DSCSR_SELECTION != previous_dscsr & DSCSR_SELECTION {
                return Err(ArmError::Other(
                    "STM32N6 SCS bank selection could not be restored".into(),
                ));
            }
            Ok(())
        })();
        match (result, restore) {
            (Ok(()), Ok(())) => {}
            (Err(error), Ok(())) | (Ok(()), Err(error)) => return Err(error),
            (Err(error), Err(restore_error)) => {
                tracing::warn!("Failed to restore STM32N6 SCS bank selection: {restore_error}");
                return Err(error);
            }
        }

        // Includes both EXTRA_S and EXTRA_NS, clearing both banks of interrupt masks.
        for reg in V8M_MAIN_SEC_FP_REGISTERS.all_registers() {
            write_core_reg(core, reg.into(), 0u32)?;
        }

        // Set thumb bit in PSR; otherwise we'll immediately fault.
        write_core_reg(
            core,
            CORTEX_M_WITH_FP_CORE_REGISTERS.psr().unwrap().into(),
            0x0100_0000_u32,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::arm::core::mock::MockProbe;

    // Ordered DAP transactions, not a model of exception delivery on silicon.
    fn interrupt_operations(previous: u32, ictr: u32) -> Vec<(bool, u64, u32)> {
        let mut ops = vec![
            (true, 0xE000_EDF0, 1 << 20), // DHCSR.S_SDE
            (true, 0xE000_EE08, previous),
        ];
        for selection in [1, 3] {
            let value = (previous & !3) | (1 << 17) | selection;
            ops.extend([
                (false, 0xE000_EE08, value),
                (true, 0xE000_EE08, value),
                (false, 0xE000_E010, 0),
                (false, 0xE000_ED04, (1 << 25) | (1 << 27)),
            ]);
        }
        ops.push((true, 0xE000_E004, ictr));
        for index in 0..=(ictr & 0xf) {
            let offset = u64::from(index) * 4;
            ops.push((false, 0xE000_E180 + offset, u32::MAX));
            ops.push((false, 0xE000_E280 + offset, u32::MAX));
        }
        ops.extend([
            (false, 0xE000_EE08, previous | (1 << 17)),
            (true, 0xE000_EE08, previous),
        ]);
        ops
    }

    fn expect_operations(core: &mut MockProbe, ops: &[(bool, u64, u32)]) {
        for &(read, address, value) in ops {
            if read {
                core.expected_read(address, value);
            } else {
                core.expected_write(address, value);
            }
        }
    }

    fn expect_reset_catch_clear(core: &mut MockProbe, previous: u32, ictr: u32) {
        expect_operations(core, &interrupt_operations(previous, ictr));

        for reg in V8M_MAIN_SEC_FP_REGISTERS.all_registers() {
            core.expected_write(0xE000_EDF8, 0);
            core.expected_write(0xE000_EDF4, 0x10000 | u32::from(reg.id().0));
            core.expected_read(0xE000_EDF0, 1 << 16);
        }
        core.expected_write(0xE000_EDF8, 1 << 24);
        core.expected_write(0xE000_EDF4, 0x10010);
        core.expected_read(0xE000_EDF0, 1 << 16);
        core.expected_write(0xE000_EF34, 0xc000_0000);
        core.expected_write(0xE000_ED14, 0x201);
        core.expected_write(0xE000_EF50, 0);
        core.expected_read(0x4602_824C, 0x100);
        core.expected_write(0x4602_824C, 0x10f);
        for address in [0x4202_3100, 0x4202_3180, 0x4202_3200, 0x4202_3280] {
            core.expected_write(address, 0);
        }
    }

    #[test]
    fn reset_catch_clear_restores_interrupt_state() {
        // One bank and the seven banks used by STM32N6. Ignore reserved ICTR bits.
        for (ictr, previous) in [
            (0, 0x30000),
            (0xffff_fff6, 0x30001),
            (6, 0x20002),
            (6, 0x20003),
        ] {
            let mut core = MockProbe::new(0);
            expect_reset_catch_clear(&mut core, previous, ictr);
            Stm32n6::create()
                .reset_catch_clear(&mut core, CoreType::Armv8m, None)
                .unwrap();

            // No later write may re-mask either security state's interrupts.
            for selector in [0x14, 0x22, 0x23] {
                let last_write =
                    core.batches.iter().rev().find(|batch| {
                        batch.get(1) == Some(&(0xE000_EDF4, Some(0x10000 | selector)))
                    });
                assert_eq!(last_write.unwrap()[0], (0xE000_EDF8, Some(0)));
            }
        }
    }

    #[test]
    fn reset_catch_clear_requires_privileged_secure_debug() {
        for dhcsr in [0, (1 << 20) | (1 << 22), (1 << 20) | (1 << 21)] {
            let mut core = MockProbe::new(0);
            core.expected_read(0xE000_EDF0, dhcsr);
            let result = Stm32n6::create().reset_catch_clear(&mut core, CoreType::Armv8m, None);
            assert!(matches!(result, Err(ArmError::Other(_))));
        }
    }

    #[test]
    fn reset_catch_clear_restores_selection_after_io_errors() {
        let ops = interrupt_operations(0x30002, 0);
        let restore = ops.len() - 2;
        for failed in 0..ops.len() {
            let mut core = MockProbe::new(0);
            expect_operations(&mut core, &ops[..failed]);
            let (read, address, _) = ops[failed];
            core.expected_error(address, read, ArmError::Timeout);
            if (2..restore).contains(&failed) {
                expect_operations(&mut core, &ops[restore..]);
            }
            let result = Stm32n6::create().reset_catch_clear(&mut core, CoreType::Armv8m, None);
            assert!(
                matches!(result, Err(ArmError::Timeout)),
                "operation {failed}: {result:?}"
            );
        }

        // A restoration failure must not replace the original cleanup error.
        let mut core = MockProbe::new(0);
        expect_operations(&mut core, &ops[..2]);
        core.expected_error(0xE000_EE08, false, ArmError::Timeout);
        core.expected_error(0xE000_EE08, false, ArmError::Other("restore failed".into()));
        let result = Stm32n6::create().reset_catch_clear(&mut core, CoreType::Armv8m, None);
        assert!(matches!(result, Err(ArmError::Timeout)));
    }

    #[test]
    fn reset_catch_clear_checks_selection_readback() {
        let ops = interrupt_operations(0x30002, 0);
        // Both bank selections and the restoration must actually take effect.
        for failed in [3, 7, ops.len() - 1] {
            let mut core = MockProbe::new(0);
            expect_operations(&mut core, &ops[..failed]);
            core.expected_read(0xE000_EE08, ops[failed].2 ^ 2);
            if failed < ops.len() - 2 {
                expect_operations(&mut core, &ops[ops.len() - 2..]);
            }
            let result = Stm32n6::create().reset_catch_clear(&mut core, CoreType::Armv8m, None);
            assert!(matches!(result, Err(ArmError::Other(_))));
        }
    }
}
