//! Sequences for the ESP32C6.

use std::{sync::Arc, time::Duration};

use crate::{
    MemoryInterface,
    architecture::riscv::{
        Dmcontrol, Riscv32,
        communication_interface::{
            MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface, Sbaddress0, Sbcs,
            Sbdata0,
        },
        sequences::RiscvDebugSequence,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
    vendor::espressif::sequences::esp::EspBreakpointHandler,
};

/// The debug sequence implementation for the ESP32C6.
#[derive(Debug)]
pub struct ESP32C6 {}

impl ESP32C6 {
    /// Creates a new debug sequence handle for the ESP32C6.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-C6 watchdogs...");
        // disable super wdt
        interface.write_word_32(0x600B1C20, 0x50D83AA1)?; // write protection off
        let current = interface.read_word_32(0x600B_1C1C)?;
        interface.write_word_32(0x600B_1C1C, current | (1 << 18))?; // set RTC_CNTL_SWD_AUTO_FEED_EN
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

    fn configure_memory_access(
        &self,
        interface: &mut RiscvCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        let memory_access_config = interface.memory_access_config();

        let accesses = [
            RiscvBusAccess::A8,
            RiscvBusAccess::A16,
            RiscvBusAccess::A32,
            RiscvBusAccess::A64,
            RiscvBusAccess::A128,
        ];
        for access in accesses {
            // External data/instruction bus
            // Loading external memory is slower than the CPU. If we can't access something via the
            // system bus, select the waiting program buffer method.
            memory_access_config.set_region_override(
                access,
                0x4200_0000..0x4300_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32C6 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        self.configure_memory_access(interface)?;
        self.disable_wdts(interface)?;

        Ok(())
    }

    fn on_halt(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface)
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        interface.halt(timeout)?;

        // System reset, ported from OpenOCD.
        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(0x600b1034))?;
        interface.write_dm_register(Sbdata0(0x80000000_u32))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0))?;

        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(0x600b1038))?;
        interface.write_dm_register(Sbdata0(0x10000000_u32))?;

        // clear dmactive to clear sbbusy otherwise debug module gets stuck
        interface.write_dm_register(Dmcontrol(0))?;

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        interface.write_dm_register(dmcontrol)?;

        std::thread::sleep(Duration::from_millis(10));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.enter_debug_mode()?;
        self.on_connect(interface)?;

        interface.reset_hart_and_halt(timeout)?;

        Ok(())
    }

    fn on_unknown_semihosting_command(
        &self,
        interface: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_riscv_idf_semihosting(interface, details)
    }
}
