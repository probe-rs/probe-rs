//! Sequences for the ESP32C3.

use std::sync::Arc;

use super::RiscvDebugSequence;
use crate::MemoryInterface;

/// The debug sequence implementation for the ESP32C3.
pub struct ESP32C3(());

impl ESP32C3 {
    /// Creates a new debug sequence handle for the ESP32C3.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self(()))
    }
}

impl RiscvDebugSequence for ESP32C3 {
    fn on_connect(
        &self,
        interface: &mut crate::architecture::riscv::communication_interface::RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling esp32c3 watchdogs...");
        // disable super wdt
        interface.write_word_32(0x600080B0, 0x8F1D312Au32)?; // write protection off
        let current = interface.read_word_32(0x600080AC)?;
        interface.write_word_32(0x600080AC, current | 1 << 31)?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600080B0, 0x0)?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6001f064, 0x50D83AA1u32)?; // write protection off
        interface.write_word_32(0x6001F048, 0x0)?;
        interface.write_word_32(0x6001f064, 0x0)?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x60020064, 0x50D83AA1u32)?; // write protection off
        interface.write_word_32(0x60020048, 0x0)?;
        interface.write_word_32(0x60020064, 0x0)?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600080a8, 0x50D83AA1u32)?; // write protection off
        interface.write_word_32(0x60008090, 0x0)?;
        interface.write_word_32(0x600080a8, 0x0)?; // write protection on

        Ok(())
    }
}
