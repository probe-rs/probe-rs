//! Sequences for the ESP32-H4.

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

const TIMG_WDT_WKEY: u32 = 0x50D8_3AA1;

// Timer Group 0/1 (IDF `DR_REG_TIMERG0/1_BASE`)
const TIMG0_WDTCONFIG0_REG: u64 = 0x6009_0000 + 0x48;
const TIMG0_WDTWPROTECT_REG: u64 = 0x6009_0000 + 0x64;
const TIMG1_WDTCONFIG0_REG: u64 = 0x6009_1000 + 0x48;
const TIMG1_WDTWPROTECT_REG: u64 = 0x6009_1000 + 0x64;

// LP WDT (IDF `DR_REG_LP_WDT_BASE`)
const LP_WDT_CONFIG0_REG: u64 = 0x600B_5400;
const LP_WDT_WPROTECT_REG: u64 = 0x600B_5400 + 0x1C;
const LP_WDT_SWD_CONFIG_REG: u64 = 0x600B_5400 + 0x20;
const LP_WDT_SWD_WPROTECT_REG: u64 = 0x600B_5400 + 0x24;
const LP_WDT_SWD_AUTO_FEED_EN: u32 = 1 << 18;

// LP AON system/CPU reset (IDF `lp_aon_reg.h`)
const LP_AON_SYS_CFG_REG: u32 = 0x600B_2800 + 0x34;
const LP_AON_CPUCORE_CFG_REG: u32 = 0x600B_2800 + 0x38;
const LP_AON_HPSYS_SW_RESET: u32 = 1 << 31;
const LP_AON_CPU_CORE0_SW_RESET: u32 = 1 << 8;

// PCR UART0 function clock
const PCR_UART0_SCLK_CONF_REG: u64 = 0x6009_4000 + 0x4;
const PCR_UART0_SCLK_EN: u32 = 1 << 22;

/// The debug sequence implementation for the ESP32-H4.
#[derive(Debug)]
pub struct ESP32H4 {}

impl ESP32H4 {
    /// Creates a new debug sequence handle for the ESP32-H4.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        tracing::info!("Disabling ESP32-H4 watchdogs...");

        // Super WDT: write-protect off, auto-feed, write-protect on
        interface.write_word_32(LP_WDT_SWD_WPROTECT_REG, TIMG_WDT_WKEY)?;
        let current = interface.read_word_32(LP_WDT_SWD_CONFIG_REG)?;
        interface.write_word_32(LP_WDT_SWD_CONFIG_REG, current | LP_WDT_SWD_AUTO_FEED_EN)?;
        interface.write_word_32(LP_WDT_SWD_WPROTECT_REG, 0x0)?;

        // TG0 WDT
        interface.write_word_32(TIMG0_WDTWPROTECT_REG, TIMG_WDT_WKEY)?;
        interface.write_word_32(TIMG0_WDTCONFIG0_REG, 0x0)?;
        interface.write_word_32(TIMG0_WDTWPROTECT_REG, 0x0)?;

        // TG1 WDT
        interface.write_word_32(TIMG1_WDTWPROTECT_REG, TIMG_WDT_WKEY)?;
        interface.write_word_32(TIMG1_WDTCONFIG0_REG, 0x0)?;
        interface.write_word_32(TIMG1_WDTWPROTECT_REG, 0x0)?;

        // LP/RTC WDT
        interface.write_word_32(LP_WDT_WPROTECT_REG, TIMG_WDT_WKEY)?;
        interface.write_word_32(LP_WDT_CONFIG0_REG, 0x0)?;
        interface.write_word_32(LP_WDT_WPROTECT_REG, 0x0)?;

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
            // External flash window (SOC_IROM/SOC_DROM 0x42000000..0x44000000)
            memory_access_config.set_region_override(
                access,
                0x4200_0000..0x4400_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32H4 {
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

        // System reset, same OpenOCD pattern as ESP32-C6/H2 with H4 LP_AON addresses.
        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(LP_AON_SYS_CFG_REG))?;
        interface.write_dm_register(Sbdata0(LP_AON_HPSYS_SW_RESET))?;

        interface.write_dm_register(Dmcontrol(0))?;

        interface.write_dm_register(Sbcs(0x48000))?;
        interface.write_dm_register(Sbaddress0(LP_AON_CPUCORE_CFG_REG))?;
        interface.write_dm_register(Sbdata0(LP_AON_CPU_CORE0_SW_RESET))?;

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

        // ROM boot needs UART0 SCLK enabled.
        let reg = interface.read_word_32(PCR_UART0_SCLK_CONF_REG)?;
        interface.write_word_32(PCR_UART0_SCLK_CONF_REG, reg | PCR_UART0_SCLK_EN)?;

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
