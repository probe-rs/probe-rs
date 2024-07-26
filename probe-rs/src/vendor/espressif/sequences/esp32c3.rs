//! Sequence for the ESP32C3.

use std::{sync::Arc, time::Duration};

use probe_rs_target::Chip;

use super::esp::EspFlashSizeDetector;
use crate::{
    architecture::riscv::{
        communication_interface::RiscvCommunicationInterface, sequences::RiscvDebugSequence,
        Dmcontrol,
    },
    MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32C3.
#[derive(Debug)]
pub struct ESP32C3 {
    inner: EspFlashSizeDetector,
}

impl ESP32C3 {
    /// Creates a new debug sequence handle for the ESP32C3.
    pub fn create(chip: &Chip) -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: EspFlashSizeDetector::stack_pointer(chip),
                load_address: 0, // Unused for RISC-V
                spiflash_peripheral: 0x6000_2000,
                attach_fn: 0x4000_0164,
            },
        })
    }

    fn disable_wdt(&self, core: &mut impl MemoryInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling esp32c3 watchdogs...");

        // disable super wdt
        core.write_word_32(0x600080B0, 0x8F1D312A)?; // write protection off
        let current = core.read_word_32(0x600080AC)?;
        core.write_word_32(0x600080AC, current | 1 << 31)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        core.write_word_32(0x600080B0, 0x0)?; // write protection on

        // tg0 wdg
        core.write_word_32(0x6001f064, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x6001F048, 0x0)?;
        core.write_word_32(0x6001f064, 0x0)?; // write protection on

        // tg1 wdg
        core.write_word_32(0x60020064, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x60020048, 0x0)?;
        core.write_word_32(0x60020064, 0x0)?; // write protection on

        // rtc wdg
        core.write_word_32(0x600080a8, 0x50D83AA1)?; // write protection off
        core.write_word_32(0x60008090, 0x0)?;
        core.write_word_32(0x600080a8, 0x0)?; // write protection on

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32C3 {
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

        // Reset all peripherals except for the RTC block.

        // At this point the core is reset and halted, ready for us to issue a system reset
        // Trigger reset via RTC_CNTL_SW_SYS_RST
        core.write_word_32(0x6000_8000, 0x9C00_A000)?;

        // Workaround for stuck in cpu start during calibration.
        core.write_word_32(0x6001_F068, 0)?;

        // Wait for the reset to take effect.
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
