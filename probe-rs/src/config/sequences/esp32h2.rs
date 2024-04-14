//! Sequences for the ESP32H2.

use std::{sync::Arc, time::Duration};

use probe_rs_target::Chip;

use crate::{
    architecture::riscv::{
        communication_interface::{RiscvCommunicationInterface, Sbaddress0, Sbcs, Sbdata0},
        sequences::RiscvDebugSequence,
        Dmcontrol,
    },
    config::sequences::esp::EspFlashSizeDetector,
    MemoryInterface,
};

/// The debug sequence implementation for the ESP32H2.
#[derive(Debug)]
pub struct ESP32H2 {
    inner: EspFlashSizeDetector,
}

impl ESP32H2 {
    /// Creates a new debug sequence handle for the ESP32H2.
    pub fn create(chip: &Chip) -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: EspFlashSizeDetector::stack_pointer(chip),
                load_address: 0, // Unused for RISC-V
                spiflash_peripheral: 0x6000_3000,
                attach_fn: 0x4000_01D4,
            },
        })
    }
}

impl RiscvDebugSequence for ESP32H2 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling esp32h2 watchdogs...");
        // disable super wdt
        interface.write_word_32(0x600B1C20, 0x50D83AA1)?; // write protection off
        let current = interface.read_word_32(0x600B_1C1C)?;
        interface.write_word_32(0x600B_1C1C, current | 1 << 18)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600B1C20, 0x0)?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6000_8064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x6000_8048, 0x0)?;
        interface.write_word_32(0x6000_8064, 0x0)?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x6000_9064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x6000_9048, 0x0)?;
        interface.write_word_32(0x6000_9064, 0x0)?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600B_1C18, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x600B_1C00, 0x0)?;
        interface.write_word_32(0x600B_1C18, 0x0)?; // write protection on

        Ok(())
    }

    fn detect_flash_size(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size_riscv(interface)
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.assert_hart_reset_and_halt(timeout)?;
        interface.deassert_hart_reset()?;

        // System reset, ported from OpenOCD.
        interface.write_word_32(0x6000_8000, 0x9C00_A000)?;

        // Workaround for stuck in cpu start during calibration.
        interface.write_word_32(0x6001_F068, 0)?;

        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(0x600b1034))?;
        interface.write_dm_register(Sbdata0(0x80000000))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0))?;

        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(0x600b1038))?;
        interface.write_dm_register(Sbdata0(0x10000000))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0))?;

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_haltreq(true);
        interface.write_dm_register(dmcontrol)?;

        interface.assert_hart_reset_and_halt(timeout)?;
        interface.deassert_hart_reset()?;

        self.on_connect(interface)?;

        Ok(())
    }
}
