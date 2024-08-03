//! Sequence for the ESP32.

use std::sync::Arc;

use probe_rs_target::Chip;

use super::esp::EspFlashSizeDetector;
use crate::{
    architecture::xtensa::{
        communication_interface::XtensaCommunicationInterface, sequences::XtensaDebugSequence,
    },
    MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32.
#[derive(Debug)]
pub struct ESP32 {
    inner: EspFlashSizeDetector,
}

impl ESP32 {
    /// Creates a new debug sequence handle for the ESP32.
    pub fn create(_chip: &Chip) -> Arc<dyn XtensaDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: 0x3FFE_0000,
                load_address: 0x400A_0000,
                spiflash_peripheral: 0x3ff4_2000,
                attach_fn: 0x4006_2a6c,
            },
        })
    }
}

impl XtensaDebugSequence for ESP32 {
    fn on_connect(&self, core: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32 watchdogs...");

        // tg0 wdg
        const TIMG0_BASE: u64 = 0x3ff5f000;
        const TIMG0_WRITE_PROT: u64 = TIMG0_BASE | 0x64;
        const TIMG0_WDTCONFIG0: u64 = TIMG0_BASE | 0x48;
        core.write_word_32(TIMG0_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(TIMG0_WDTCONFIG0, 0x0)?;
        core.write_word_32(TIMG0_WRITE_PROT, 0x0)?; // write protection on

        // tg1 wdg
        const TIMG1_BASE: u64 = 0x3ff60000;
        const TIMG1_WRITE_PROT: u64 = TIMG1_BASE | 0x64;
        const TIMG1_WDTCONFIG0: u64 = TIMG1_BASE | 0x48;
        core.write_word_32(TIMG1_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(TIMG1_WDTCONFIG0, 0x0)?;
        core.write_word_32(TIMG1_WRITE_PROT, 0x0)?; // write protection on

        // rtc wdg
        const RTC_CNTL_BASE: u64 = 0x3ff48000;
        const RTC_WRITE_PROT: u64 = RTC_CNTL_BASE | 0xa4;
        const RTC_WDTCONFIG0: u64 = RTC_CNTL_BASE | 0x8c;
        core.write_word_32(RTC_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(RTC_WDTCONFIG0, 0x0)?;
        core.write_word_32(RTC_WRITE_PROT, 0x0)?; // write protection on

        tracing::warn!("Be careful not to reset your ESP32 while connected to the debugger! Depending on the specific device, this may render it temporarily inoperable or permanently damage it.");

        Ok(())
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size_esp32(session)
    }
}
