//! Sequence for the ESP32C3.

use std::{sync::Arc, time::Duration};

use super::esp::EspFlashSizeDetector;
use crate::{
    MemoryInterface, Session,
    architecture::riscv::{
        Dmcontrol, Dmstatus, Riscv32, communication_interface::RiscvCommunicationInterface,
        sequences::RiscvDebugSequence,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
    vendor::espressif::sequences::esp::EspBreakpointHandler,
};

/// The debug sequence implementation for the ESP32C3.
#[derive(Debug)]
pub struct ESP32C3 {
    inner: EspFlashSizeDetector,
}

impl ESP32C3 {
    /// Creates a new debug sequence handle for the ESP32C3.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: 0x403c0000,
                load_address: 0x40390000,
                spiflash_peripheral: 0x6000_2000,
                efuse_get_spiconfig_fn: Some(0x4000071c),
                attach_fn: 0x4000_0164,
            },
        })
    }

    async fn disable_wdts(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-C3 watchdogs...");

        // FIXME: this is a terrible hack because we should not need to halt to read memory.
        interface.sysbus_requires_halting(true);

        // disable super wdt
        interface.write_word_32(0x600080B0, 0x8F1D312A).await?; // write protection off
        let current = interface.read_word_32(0x600080AC).await?;
        interface
            .write_word_32(0x600080AC, current | (1 << 31))
            .await?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600080B0, 0x0).await?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6001f064, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x6001F048, 0x0).await?;
        interface.write_word_32(0x6001f064, 0x0).await?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x60020064, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x60020048, 0x0).await?;
        interface.write_word_32(0x60020064, 0x0).await?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600080a8, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x60008090, 0x0).await?;
        interface.write_word_32(0x600080a8, 0x0).await?; // write protection on

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl RiscvDebugSequence for ESP32C3 {
   async fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface).await
    }

    async fn on_halt(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface).await
    }

    async fn detect_flash_size(
        &self,
        session: &mut Session,
    ) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size(session).await
    }

    async fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.halt(timeout).await?;

        // Reset all peripherals except for the RTC block.

        // At this point the core is reset and halted, ready for us to issue a system reset
        // Trigger reset via RTC_CNTL_SW_SYS_RST
        interface.write_word_32(0x6000_8000, 0x9C00_A000).await?;

        // Workaround for stuck in cpu start during calibration.
        interface.write_word_32(0x6001_F068, 0).await?;

        // Wait for the reset to take effect.
        loop {
            let dmstatus = interface.read_dm_register::<Dmstatus>().await?;
            if dmstatus.allhavereset() && dmstatus.allhalted() {
                break;
            }
        }

        // Clear allhavereset and anyhavereset
        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol).await?;

        interface.reset_hart_and_halt(timeout).await?;

        self.on_connect(interface).await?;

        Ok(())
    }

    async fn on_unknown_semihosting_command(
        &self,
        interface: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_riscv_idf_semihosting(interface, details).await
    }
}
