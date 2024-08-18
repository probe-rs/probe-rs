//! Sequence for the ESP32C3.

use std::{sync::Arc, time::Duration};

use probe_rs_target::Chip;

use super::esp::EspFlashSizeDetector;
use crate::{
    architecture::riscv::{
        communication_interface::RiscvCommunicationInterface, sequences::RiscvDebugSequence,
        Dmcontrol, Dmstatus,
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
}

impl RiscvDebugSequence for ESP32C3 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling esp32c3 watchdogs...");

        // disable super wdt
        interface.write_word_32(0x600080B0, 0x8F1D312A)?; // write protection off
        let current = interface.read_word_32(0x600080AC)?;
        interface.write_word_32(0x600080AC, current | 1 << 31)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600080B0, 0x0)?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6001f064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x6001F048, 0x0)?;
        interface.write_word_32(0x6001f064, 0x0)?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x60020064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x60020048, 0x0)?;
        interface.write_word_32(0x60020064, 0x0)?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600080a8, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x60008090, 0x0)?;
        interface.write_word_32(0x600080a8, 0x0)?; // write protection on

        Ok(())
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size(session)
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.halt(timeout)?;

        // Reset all peripherals except for the RTC block.

        // At this point the core is reset and halted, ready for us to issue a system reset
        // Trigger reset via RTC_CNTL_SW_SYS_RST
        interface.write_word_32(0x6000_8000, 0x9C00_A000)?;

        // Workaround for stuck in cpu start during calibration.
        interface.write_word_32(0x6001_F068, 0)?;

        // Wait for the reset to take effect.
        loop {
            let dmstatus = interface.read_dm_register::<Dmstatus>()?;
            if dmstatus.allhavereset() && dmstatus.allhalted() {
                break;
            }
        }

        // Clear allhavereset and anyhavereset
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.reset_hart_and_halt(timeout)?;

        self.on_connect(interface)?;

        Ok(())
    }
}
