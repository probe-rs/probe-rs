//! Sequence for the ESP32.

use std::sync::Arc;

use probe_rs_target::Chip;

use super::XtensaDebugSequence;
use crate::{
    architecture::xtensa::communication_interface::XtensaCommunicationInterface, MemoryInterface,
};

/// The debug sequence implementation for the ESP32.
#[derive(Debug)]
pub struct ESP32 {}

impl ESP32 {
    /// Creates a new debug sequence handle for the ESP32.
    pub fn create(_chip: &Chip) -> Arc<dyn XtensaDebugSequence> {
        Arc::new(Self {})
    }
}

impl XtensaDebugSequence for ESP32 {
    fn on_connect(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32 watchdogs...");

        // tg0 wdg
        const TIMG0_BASE: u64 = 0x3ff5f000;
        const TIMG0_WRITE_PROT: u64 = TIMG0_BASE | 0x64;
        const TIMG0_WDTCONFIG0: u64 = TIMG0_BASE | 0x48;
        interface.write_word_32(TIMG0_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(TIMG0_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG0_WRITE_PROT, 0x0)?; // write protection on

        // tg1 wdg
        const TIMG1_BASE: u64 = 0x3ff60000;
        const TIMG1_WRITE_PROT: u64 = TIMG1_BASE | 0x64;
        const TIMG1_WDTCONFIG0: u64 = TIMG1_BASE | 0x48;
        interface.write_word_32(TIMG1_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(TIMG1_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG1_WRITE_PROT, 0x0)?; // write protection on

        // rtc wdg
        const RTC_CNTL_BASE: u64 = 0x3ff48000;
        const RTC_WRITE_PROT: u64 = RTC_CNTL_BASE | 0xa4;
        const RTC_WDTCONFIG0: u64 = RTC_CNTL_BASE | 0x8c;
        interface.write_word_32(RTC_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(RTC_WDTCONFIG0, 0x0)?;
        interface.write_word_32(RTC_WRITE_PROT, 0x0)?; // write protection on

        Ok(())
    }
}
