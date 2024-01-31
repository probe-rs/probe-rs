//! Sequences for the ESP32H2.

use std::sync::Arc;

use espflash::flasher::FlashSize;
use espflash::targets::XtalFrequency;
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

    fn as_esp_sequence(
        &self,
    ) -> Option<&dyn EspDebugSequence<Interface = RiscvCommunicationInterface>> {
        Some(self)
    }
}

impl EspDebugSequence for ESP32H2 {
    type Interface = RiscvCommunicationInterface;

    fn detect_flash_size(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<Option<FlashSize>, Error> {
        self.inner.detect_flash_size_riscv(interface)
    }

    fn detect_xtal_frequency(
        &self,
        _interface: &mut Self::Interface,
    ) -> Result<XtalFrequency, Error> {
        Ok(XtalFrequency::_32Mhz)
    }
}
