//! Sequences for the ESP32P4.

use std::sync::Arc;

use crate::{
    architecture::riscv::{
        communication_interface::RiscvCommunicationInterface, sequences::RiscvDebugSequence,
    },
    MemoryInterface, Session,
};

/// The debug sequence implementation for the ESP32P4.
#[derive(Debug)]
pub struct ESP32P4;

impl ESP32P4 {
    /// Creates a new debug sequence handle for the ESP32P4.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }
}

impl RiscvDebugSequence for ESP32P4 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32P4 watchdogs...");
        // tg0 wdg
        interface.write_word_32(0x500c2064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x500c2048, 0x0)?;
        interface.write_word_32(0x500c207c, 0x4)?; // clear interrupt state
        interface.write_word_32(0x500c2064, 0x0)?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x500c3064, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x500c3048, 0x0)?;
        interface.write_word_32(0x500c307c, 0x4)?; // clear interrupt state
        interface.write_word_32(0x500c3064, 0x0)?; // write protection on

        // rtc wdg
        interface.write_word_32(0x50116018, 0x50D83AA1)?; // write protection off
        interface.write_word_32(0x50116000, 0x0)?;
        interface.write_word_32(0x50116030, 0xC0000000)?; // clear interrupt state
        interface.write_word_32(0x50116018, 0x0)?; // write protection on

        Ok(())
    }

    fn detect_flash_size(&self, _interface: &mut Session) -> Result<Option<usize>, crate::Error> {
        Ok(None)
    }
}
