//! Sequence for the ESP32C3.

use std::sync::Arc;

use espflash::flasher::FlashSize;
use probe_rs_target::Chip;

use crate::{
    architecture::{
        esp32::EspDebugSequence,
        riscv::{
            communication_interface::RiscvCommunicationInterface, sequences::RiscvDebugSequence,
        },
    },
    config::sequences::esp::EspFlashSizeDetector,
    Error, MemoryInterface,
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

    fn as_esp_sequence(
        &self,
    ) -> Option<&dyn EspDebugSequence<Interface = RiscvCommunicationInterface>> {
        Some(self)
    }
}

impl EspDebugSequence for ESP32C3 {
    type Interface = RiscvCommunicationInterface;

    fn detect_flash_size(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<FlashSize>, Error> {
        self.inner.detect_flash_size_riscv(interface)
    }
}
