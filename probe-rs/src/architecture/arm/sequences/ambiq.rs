use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Result;

use super::ArmDebugSequence;
use crate::{
    architecture::arm::core::{self, register},
    core::CoreRegister,
    DebugProbeError, Memory,
};

pub struct AMA3B(());

impl AMA3B {
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for AMA3B {
    fn debug_device_unlock(&self, interface: &mut crate::Memory) -> Result<(), crate::Error> {
        // set C runtime environment
        //
        // From: https://github.com/sparkfun/SparkFun_Apollo3_AmbiqSuite_BSPs/blob/6398086a1a87ddea78274521683ba3ad817bee82/common/tools_sfe/embedded/jlink-prog-combined.txt
        log::info!("ambiq: set C runtime");
        interface.write_core_reg(core::armv7m::MSP, 0x10000100)?;
        Ok(())
    }

    fn reset_and_halt(&self, interface: &mut Memory) -> Result<(), crate::Error> {
        // Based on:
        //
        //   - https://github.com/sparkfun/AmbiqSuiteSDK/blob/e280cbde3e366509da6768ab95471782a05d2371/tools/apollo3_scripts/AMA3B2KK-KBR.JLinkScript
        //
        // Set the vc_corereset bit in the DEMCR register.
        // This will halt the core after reset.
        const AIRCR_ADDR: u32 = 0xE000ED0C;
        const DHCSR_ADDR: u32 = 0xE000EDF0;
        const DEMCR_ADDR: u32 = 0xE000EDFC;
        const MCUCTRL_SCRATCH0: u32 = 0x400401B0;
        const MCUCTRL_BOOTLDR: u32 = 0x400401A0;
        const JDEC_PID: u32 = 0xF0000FE0;

        // check if we are in secure mode
        let r = interface.read_word_32(MCUCTRL_BOOTLDR.into())?;
        let secure = (r & 0x0C00_0000) == 0x0400_0000;
        log::debug!("Ambiq bootload secure mode: {}", secure);

        if secure {
            // Set MCUCTRL Scratch0, indicating that the Bootloader needs to run, then halt when it is finished.
            log::debug!(
                "Secure mode: bootloader needs to run, and will halt when it has finished."
            );
            let scratch0 = interface.read_word_32(MCUCTRL_SCRATCH0.into())?;
            log::trace!("scratch0: {:#x}", scratch0);

            let scratch0 = scratch0 | 0x1;
            interface.write_word_32(MCUCTRL_SCRATCH0.into(), scratch0)?;
        } else {
            unimplemented!("Non-secure bootloader not implemented yet.")
        }

        // Request MCU to reset
        log::debug!("requesting MCU to reset..");
        interface.write_word_32(AIRCR_ADDR, 0x05FA0004)?;
        std::thread::sleep(Duration::from_millis(1000));

        // wait for reset
        while {
            let v = interface.read_word_32(DHCSR_ADDR)?;
            !((v != 0xFFFFFFFF) && (v & 0x00020000) > 0)
        } {
            std::thread::sleep(Duration::from_millis(1000));
            log::debug!("waiting for reset..");
        }

        const XPSR_THUMB: u32 = 1 << 24;
        let xpsr_value = interface.read_core_reg(register::XPSR.address)?;
        if xpsr_value & XPSR_THUMB == 0 {
            interface.write_core_reg(register::XPSR.address, xpsr_value | XPSR_THUMB)?;
        }

        Ok(())
    }

    fn reset_system(&self, interface: &mut Memory) -> Result<(), crate::Error> {
        use crate::architecture::arm::core::armv7m::{Aircr, Dhcsr};

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::ADDRESS, aircr.into())?;

        let start = Instant::now();

        while start.elapsed() < Duration::from_micros(50_0000) {
            thread::sleep(std::time::Duration::from_millis(100));
            let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::ADDRESS)?);

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                return Ok(());
            }
        }

        Err(crate::Error::Probe(DebugProbeError::Timeout))
    }
}
