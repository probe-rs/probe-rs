//! Sequences for the ESP32C6.

use std::{sync::Arc, time::Duration};

use super::esp::EspFlashSizeDetector;
use crate::{
    MemoryInterface, Session,
    architecture::riscv::{
        Dmcontrol, Riscv32,
        communication_interface::{RiscvCommunicationInterface, Sbaddress0, Sbcs, Sbdata0},
        sequences::RiscvDebugSequence,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
    vendor::espressif::sequences::esp::EspBreakpointHandler,
};

/// The debug sequence implementation for the ESP32C6.
#[derive(Debug)]
pub struct ESP32C6 {
    inner: EspFlashSizeDetector,
}

impl ESP32C6 {
    /// Creates a new debug sequence handle for the ESP32C6.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: 0x40850000,
                load_address: 0x40810000,
                spiflash_peripheral: 0x6000_3000,
                efuse_get_spiconfig_fn: None,
                attach_fn: 0x4000_01DC,
            },
        })
    }

    async fn disable_wdts(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-C6 watchdogs...");
        // disable super wdt
        interface.write_word_32(0x600B1C20, 0x50D83AA1).await?; // write protection off
        let current = interface.read_word_32(0x600B_1C1C).await?;
        interface
            .write_word_32(0x600B_1C1C, current | (1 << 18))
            .await?; // set RTC_CNTL_SWD_AUTO_FEED_EN
        interface.write_word_32(0x600B1C20, 0x0).await?; // write protection on

        // tg0 wdg
        interface.write_word_32(0x6000_8064, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x6000_8048, 0x0).await?;
        interface.write_word_32(0x6000_8064, 0x0).await?; // write protection on

        // tg1 wdg
        interface.write_word_32(0x6000_9064, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x6000_9048, 0x0).await?;
        interface.write_word_32(0x6000_9064, 0x0).await?; // write protection on

        // rtc wdg
        interface.write_word_32(0x600B_1C18, 0x50D83AA1).await?; // write protection off
        interface.write_word_32(0x600B_1C00, 0x0).await?;
        interface.write_word_32(0x600B_1C18, 0x0).await?; // write protection on

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl RiscvDebugSequence for ESP32C6 {
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

        // System reset, ported from OpenOCD.
        interface.write_dm_register(Sbcs(0x48000)).await?;
        interface.write_dm_register(Sbaddress0(0x600b1034)).await?;
        interface.write_dm_register(Sbdata0(0x80000000_u32)).await?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0)).await?;

        interface.write_dm_register(Sbcs(0x48000)).await?;
        interface.write_dm_register(Sbaddress0(0x600b1038)).await?;
        interface.write_dm_register(Sbdata0(0x10000000_u32)).await?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0)).await?;

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        interface.write_dm_register(dmcontrol).await?;

        std::thread::sleep(Duration::from_millis(10));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol).await?;

        interface.enter_debug_mode().await?;
        self.on_connect(interface).await?;

        interface.reset_hart_and_halt(timeout).await?;

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
