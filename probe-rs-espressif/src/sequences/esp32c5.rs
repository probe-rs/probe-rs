//! Sequences for the ESP32C5.

use std::{sync::Arc, time::Duration};

use crate::sequences::esp::EspBreakpointHandler;
use probe_rs::{
    Error, MemoryInterface,
    architecture::riscv::{
        Dmcontrol, Riscv32,
        communication_interface::{
            MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface, Sbaddress0, Sbcs,
            Sbdata0,
        },
        sequences::RiscvDebugSequence,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
};

/// The debug sequence implementation for the ESP32C5.
#[derive(Debug)]
pub struct ESP32C5 {}

impl ESP32C5 {
    /// Creates a new debug sequence handle for the ESP32C5.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        tracing::info!("Disabling ESP32-C5 watchdogs...");
        // disable LP_WDT_SWD
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
    ) -> Result<(), Error> {
        let memory_access_config = interface.memory_access_config();

        let accesses = [
            RiscvBusAccess::A8,
            RiscvBusAccess::A16,
            RiscvBusAccess::A32,
            RiscvBusAccess::A64,
            RiscvBusAccess::A128,
        ];
        for access in accesses {
            // CPU subsystem
            memory_access_config.set_region_override(
                access,
                0x2000_0000..0x3000_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );
            // External data/instruction bus
            // Loading external memory is slower than the CPU. If we can't access something via the
            // system bus, select the waiting program buffer method.
            memory_access_config.set_region_override(
                access,
                0x4200_0000..0x4400_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32C5 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        self.configure_memory_access(interface)?;
        self.disable_wdts(interface)?;

        Ok(())
    }

    fn on_halt(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        self.disable_wdts(interface)
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), Error> {
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

        fn set_bit(
            interface: &mut RiscvCommunicationInterface,
            addr: u64,
            bit: usize,
        ) -> Result<(), Error> {
            let reg = interface.read_word_32(addr)?;
            interface.write_word_32(addr, reg | (1u32 << bit))
        }

        fn clear_bit(
            interface: &mut RiscvCommunicationInterface,
            addr: u64,
            bit: usize,
        ) -> Result<(), Error> {
            let reg = interface.read_word_32(addr)?;
            interface.write_word_32(addr, reg & !(1u32 << bit))
        }

        // Reset modem
        const MODEM_SYSCON_MODEM_RST_CONF: u64 = 0x600A9C00 + 0x10;
        interface.write_word_32(MODEM_SYSCON_MODEM_RST_CONF, 0xFFFF_FFFF)?;
        interface.write_word_32(MODEM_SYSCON_MODEM_RST_CONF, 0)?;
        const MODEM_LPCON_RST_CONF: u64 = 0x600AF000 + 0x24;
        interface.write_word_32(MODEM_LPCON_RST_CONF, 0xFF)?;
        interface.write_word_32(MODEM_LPCON_RST_CONF, 0)?;

        // Reset peripherals
        const PCR_BASE: u64 = 0x6009_6000;

        const MSPI_CLK_CONF_OFFSET: u64 = 0x1c;
        const MSPI_CONF_OFFSET: u64 = 0x18;
        const UART0_SCLK_CONF_OFFSET: u64 = 0x04;

        const PCR_PERI_REGISTER_OFFSETS: &[u64] = &[
            0x00,  // UART0_CONF
            0x0c,  // UART1_CONF
            0x6c,  // SYSTIMER_CONF
            0xc0,  // GDMA_CONF
            0x108, // MODEM_CONF
            0xa4,  // PWM_CONF
            0x17c, // SDIO_SLAVE_CONF
            0xa0,  // ETM_CONF
            0xf8,  // REGDMA_CONF
            0xcc,  // AES_CONF
            0xe4,  // DS_CONF
            0xdc,  // ECC_CONF
            0xec,  // ECDSA_CONF
            0xe8,  // HMAC_CONF
            0xd4,  // RSA_CONF
            0xd0,  // SHA_CONF
        ];

        // Must reset mspi AXI before reset mspi core.
        set_bit(interface, PCR_BASE + MSPI_CLK_CONF_OFFSET, 11)?;
        set_bit(interface, PCR_BASE + MSPI_CONF_OFFSET, 1)?;
        // Must release mspi core reset before mspi AXI.
        clear_bit(interface, PCR_BASE + MSPI_CONF_OFFSET, 1)?;
        clear_bit(interface, PCR_BASE + MSPI_CLK_CONF_OFFSET, 11)?;

        for offset in PCR_PERI_REGISTER_OFFSETS {
            set_bit(interface, PCR_BASE + *offset, 1)?;
            clear_bit(interface, PCR_BASE + *offset, 1)?;
        }

        // The ROM code fails to boot if UART0 SCLK is not enabled
        set_bit(interface, PCR_BASE + UART0_SCLK_CONF_OFFSET, 22)?;

        interface.reset_hart_and_halt(timeout)?;

        Ok(())
    }

    fn on_unknown_semihosting_command(
        &self,
        interface: &mut Riscv32,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, Error> {
        EspBreakpointHandler::handle_riscv_idf_semihosting(interface, details)
    }
}
