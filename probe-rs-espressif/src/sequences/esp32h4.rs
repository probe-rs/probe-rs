//! Sequences for the ESP32H4.

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

const WDT_WKEY: u32 = 0x50D8_3AA1;

const TIMG0_WDTCONFIG0: u64 = 0x6009_0048;
const TIMG0_WDTWPROTECT: u64 = 0x6009_0064;
const TIMG1_WDTCONFIG0: u64 = 0x6009_1048;
const TIMG1_WDTWPROTECT: u64 = 0x6009_1064;

const LP_WDT_CONFIG0: u64 = 0x600B_5400;
const LP_WDT_WPROTECT: u64 = 0x600B_541C;
const LP_WDT_SWD_CONFIG: u64 = 0x600B_5420;
const LP_WDT_SWD_WPROTECT: u64 = 0x600B_5424;
const LP_WDT_SWD_AUTO_FEED_EN: u32 = 1 << 18;

const LP_AON_SYS_CFG: u32 = 0x600B_2834;
const LP_AON_CPUCORE_CFG: u32 = 0x600B_2838;
const LP_AON_HPSYS_SW_RESET: u32 = 1 << 31;
const LP_AON_CPU_UNSTALL: u32 = 0x00FF_00FF;
const LP_AON_CPU_RESET: u32 = 0x01FF_01FF;

const PCR_CORE1_CONF: u64 = 0x6009_4188;
const PCR_CORE1_CLK_EN: u32 = 1 << 0;
const PCR_UART0_SCLK_CONF: u64 = 0x6009_4004;
const PCR_UART0_SCLK_EN: u32 = 1 << 22;

/// The debug sequence implementation for the ESP32H4.
#[derive(Debug)]
pub struct ESP32H4 {}

impl ESP32H4 {
    /// Creates a new debug sequence handle for the ESP32H4.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        tracing::info!("Disabling ESP32-H4 watchdogs...");

        // Super WDT
        interface.write_word_32(LP_WDT_SWD_WPROTECT, WDT_WKEY)?;
        let current = interface.read_word_32(LP_WDT_SWD_CONFIG)?;
        interface.write_word_32(LP_WDT_SWD_CONFIG, current | LP_WDT_SWD_AUTO_FEED_EN)?;
        interface.write_word_32(LP_WDT_SWD_WPROTECT, 0x0)?;

        // TG0 WDT
        interface.write_word_32(TIMG0_WDTWPROTECT, WDT_WKEY)?;
        interface.write_word_32(TIMG0_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG0_WDTWPROTECT, 0x0)?;

        // TG1 WDT
        interface.write_word_32(TIMG1_WDTWPROTECT, WDT_WKEY)?;
        interface.write_word_32(TIMG1_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG1_WDTWPROTECT, 0x0)?;

        // RTC WDT
        interface.write_word_32(LP_WDT_WPROTECT, WDT_WKEY)?;
        interface.write_word_32(LP_WDT_CONFIG0, 0x0)?;
        interface.write_word_32(LP_WDT_WPROTECT, 0x0)?;

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
            // External flash window
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

        // System reset, ported from OpenOCD.
        interface.write_dm_register(Sbcs(0x40000))?;

        interface.write_dm_register(Sbaddress0(LP_AON_CPUCORE_CFG))?;
        interface.write_dm_register(Sbdata0(LP_AON_CPU_UNSTALL))?;

        interface.write_dm_register(Sbcs(0x140000))?;
        interface.write_dm_register(Sbaddress0(LP_AON_SYS_CFG))?;
        let reg_val = interface.read_dm_register::<Sbdata0>()?.0;
        interface.write_dm_register(Sbcs(0x40000))?;
        interface.write_dm_register(Sbaddress0(LP_AON_SYS_CFG))?;
        interface.write_dm_register(Sbdata0(reg_val | LP_AON_HPSYS_SW_RESET))?;

        interface.write_dm_register(Sbaddress0(LP_AON_CPUCORE_CFG))?;
        interface.write_dm_register(Sbdata0(LP_AON_CPU_RESET))?;

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_resumereq(true);
        interface.write_dm_register(dmcontrol)?;

        std::thread::sleep(Duration::from_millis(100));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.enter_debug_mode()?;
        self.on_connect(interface)?;

        interface.write_word_32(PCR_CORE1_CONF, PCR_CORE1_CLK_EN)?;

        let reg = interface.read_word_32(PCR_UART0_SCLK_CONF)?;
        interface.write_word_32(PCR_UART0_SCLK_CONF, reg | PCR_UART0_SCLK_EN)?;

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
