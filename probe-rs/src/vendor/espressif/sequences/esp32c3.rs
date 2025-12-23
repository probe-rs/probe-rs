//! Sequence for the ESP32C3.

use std::{sync::Arc, time::Duration};

use crate::{
    MemoryInterface,
    architecture::riscv::{
        Dmcontrol, Dmstatus, Riscv32,
        communication_interface::{
            MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface,
        },
        sequences::RiscvDebugSequence,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
    vendor::espressif::sequences::esp::EspBreakpointHandler,
};

/// The debug sequence implementation for the ESP32C3.
#[derive(Debug)]
pub struct ESP32C3 {}

impl ESP32C3 {
    /// Creates a new debug sequence handle for the ESP32C3.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-C3 watchdogs...");

        // disable super wdt
        interface.write_word_32(0x600080B0, 0x8F1D312A)?; // write protection off
        let current = interface.read_word_32(0x600080AC)?;
        interface.write_word_32(0x600080AC, current | (1 << 31))?; // set RTC_CNTL_SWD_AUTO_FEED_EN
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
            let method = memory_access_config.default_method(access);

            // FIXME: this is a terrible hack because we should not need to halt to read memory.
            memory_access_config
                .set_default_method(access, method.min(MemoryAccessMethod::HaltedSystemBus));

            if method != MemoryAccessMethod::SystemBus {
                // External data bus
                // Loading external memory is slower than the CPU. If we can't access something via the
                // system bus, select the waiting program buffer method.
                memory_access_config.set_region_override(
                    access,
                    0x3C00_0000..0x3C80_0000,
                    MemoryAccessMethod::WaitingProgramBuffer,
                );
            }
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32C3 {
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

    fn on_unknown_semihosting_command(
        &self,
        interface: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_riscv_idf_semihosting(interface, details)
    }
}
