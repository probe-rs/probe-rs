//! Sequences for the ESP32C6.

use std::{sync::Arc, time::Duration};

use probe_rs_target::Chip;

use super::esp::EspFlashSizeDetector;
use crate::{
    architecture::riscv::{
        communication_interface::{RiscvCommunicationInterface, Sbaddress0, Sbcs, Sbdata0},
        sequences::RiscvDebugSequence,
        Dmcontrol,
    },
    MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32C6.
#[derive(Debug)]
pub struct ESP32C6 {
    inner: EspFlashSizeDetector,
}

impl ESP32C6 {
    /// Creates a new debug sequence handle for the ESP32C6.
    pub fn create(chip: &Chip) -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: EspFlashSizeDetector::stack_pointer(chip),
                load_address: 0, // Unused for RISC-V
                spiflash_peripheral: 0x6000_3000,
                attach_fn: 0x4000_01DC,
            },
        })
    }

    fn disable_wdt(&self, core: &mut impl MemoryInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling esp32c6 watchdogs...");

        // disable super wdt
        core.write_word_32(0x600B1C20, 0x50D83AA1)?; // write protection off
        let current = core.read_word_32(0x600B_1C1C)?;
        core.write_word_32(0x600B_1C1C, current | 1 << 18)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        core.write_word_32(0x600B1C20, 0x0)?; // write protection on

        // tg0 wdg
        core.write_word_32(0x6000_8064, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x6000_8048, 0x0)?;
        core.write_word_32(0x6000_8064, 0x0)?; // write protection on

        // tg1 wdg
        core.write_word_32(0x6000_9064, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x6000_9048, 0x0)?;
        core.write_word_32(0x6000_9064, 0x0)?; // write protection on

        // rtc wdg
        core.write_word_32(0x600B_1C18, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x600B_1C00, 0x0)?;
        core.write_word_32(0x600B_1C18, 0x0)?; // write protection on

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32C6 {
    fn on_connect(&self, session: &mut Session) -> Result<(), crate::Error> {
        let mut core = session.core(0).unwrap();

        self.disable_wdt(&mut core)
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size(session)
    }

    fn reset_system_and_halt(
        &self,
        core: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        core.halt(timeout)?;

        // System reset, ported from OpenOCD.
        core.write_dm_register(Sbcs(0x48000))?;
        core.write_dm_register(Sbaddress0(0x600b1034))?;
        core.write_dm_register(Sbdata0(0x80000000_u32))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        core.write_dm_register(Dmcontrol(0))?;

        core.write_dm_register(Sbcs(0x48000))?;
        core.write_dm_register(Sbaddress0(0x600b1038))?;
        core.write_dm_register(Sbdata0(0x10000000_u32))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        core.write_dm_register(Dmcontrol(0))?;

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        core.write_dm_register(dmcontrol)?;

        std::thread::sleep(Duration::from_millis(10));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        core.write_dm_register(dmcontrol)?;

        core.enter_debug_mode()?;
        self.disable_wdt(core)?;

        core.reset_hart_and_halt(timeout)?;

        Ok(())
    }
}
