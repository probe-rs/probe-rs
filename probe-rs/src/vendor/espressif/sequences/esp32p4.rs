//! Sequences for the ESP32P4.

use std::{sync::Arc, time::Duration};

use crate::{
    MemoryInterface,
    architecture::riscv::{
        Dmcontrol,
        communication_interface::{
            MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface, Sbaddress0, Sbcs,
            Sbdata0,
        },
        sequences::RiscvDebugSequence,
    },
};

const DR_REG_LP_WDT_BASE: u64 = 0x5011_6000;

const RTC_CNTL_WDTCONFIG0_REG: u64 = DR_REG_LP_WDT_BASE;
const RTC_CNTL_WDTWPROTECT_REG: u64 = DR_REG_LP_WDT_BASE + 0x18;
const RTC_CNTL_WDT_WKEY: u32 = 0x50d8_3aa1;

const RTC_CNTL_SWD_CONF_REG: u64 = DR_REG_LP_WDT_BASE + 0x001c;
const RTC_CNTL_SWD_AUTO_FEED_EN: u32 = 1 << 18;
const RTC_CNTL_SWD_WPROTECT_REG: u64 = DR_REG_LP_WDT_BASE + 0x0020;
const RTC_CNTL_SWD_WKEY: u32 = 0x50d8_3aa1;

const DR_REG_TIMG0_BASE: u64 = 0x500c_2000;
const TIMG0_WDTCONFIG0_REG: u64 = DR_REG_TIMG0_BASE + 0x48;
const TIMG0_WDTWPROTECT_REG: u64 = DR_REG_TIMG0_BASE + 0x64;
const TIMG0_WDT_WKEY: u32 = 0x50d8_3aa1;
const TIMG0_INT_CLR_TIMERS_REG: u64 = DR_REG_TIMG0_BASE + 0x7c;
const TIMG0_INT_CLR_WDG_INT: u32 = 0x4;

const DR_REG_TIMG1_BASE: u64 = 0x500c_3000;
const TIMG1_WDTCONFIG0_REG: u64 = DR_REG_TIMG1_BASE + 0x48;
const TIMG1_WDTWPROTECT_REG: u64 = DR_REG_TIMG1_BASE + 0x64;
const TIMG1_WDT_WKEY: u32 = 0x50d8_3aa1;
const TIMG1_INT_CLR_TIMERS_REG: u64 = DR_REG_TIMG1_BASE + 0x7c;
const TIMG1_INT_CLR_WDG_INT: u32 = 0x4;

/// A register that effects a reset when bits are set, and releases the
/// block from reset when `0` is written.
struct ResetRegister {
    /// The physical address where this reset occurs
    address: u64,
    /// The value of legal bits to set to trigger the reset
    value: u32,
}

impl ResetRegister {
    const fn new(address: u64, value: u32) -> ResetRegister {
        ResetRegister { address, value }
    }
}
/// Manually reset blocks, since there's no way to otherwise perform
/// a global reset.
const HP_RST_EN0_REG: ResetRegister = ResetRegister::new(
    0x500e_60c0,
    // Skip REG_RST_EN_CORE0_GLOBAL and REG_RST_EN_CORE1_GLOBAL since
    // we're already resetting the CPUs
    !(1 << 7 | 1 << 8),
);
const HP_RST_EN1_REG: ResetRegister = ResetRegister::new(0x500e_60c4, !0);
const HP_RST_EN2_REG: ResetRegister = ResetRegister::new(0x500e_60c8, !0);

const LP_AONCLKRST_LP_RST_EN: ResetRegister = ResetRegister::new(
    0x5011_100c,
    // Skip resetting the efuse registers, since that will reset the USJ connection
    0xfffc_0000 & !(1 << 30),
);

const LP_PERI_RESET_EN: ResetRegister = ResetRegister::new(0x5012_0008, 0xfffc_0000);

/// The debug sequence implementation for the ESP32P4.
#[derive(Debug)]
pub struct ESP32P4 {}

impl ESP32P4 {
    /// Creates a new debug sequence handle for the ESP32P4.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(
        &self,
        interface: &mut RiscvCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32P4 watchdogs...");

        // tg0 wdg
        // Write protection off
        interface.write_word_32(TIMG0_WDTWPROTECT_REG, TIMG0_WDT_WKEY)?;
        // Disable reset
        interface.write_word_32(TIMG0_WDTCONFIG0_REG, 0x0)?;
        // Clear interrupt state
        interface.write_word_32(TIMG0_INT_CLR_TIMERS_REG, TIMG0_INT_CLR_WDG_INT)?;
        // Write protection on
        interface.write_word_32(TIMG0_WDTWPROTECT_REG, 0x0)?;

        // tg1 wdg
        // Write protection off
        interface.write_word_32(TIMG1_WDTWPROTECT_REG, TIMG1_WDT_WKEY)?;
        // Disable reset
        interface.write_word_32(TIMG1_WDTCONFIG0_REG, 0x0)?;
        // Clear interrupt state
        interface.write_word_32(TIMG1_INT_CLR_TIMERS_REG, TIMG1_INT_CLR_WDG_INT)?;
        // Write protection on
        interface.write_word_32(TIMG1_WDTWPROTECT_REG, 0x0)?;

        // Disable RTC WDT
        // Write protection off
        interface.write_word_32(RTC_CNTL_WDTWPROTECT_REG, RTC_CNTL_WDT_WKEY)?;
        // Disable reset
        interface.write_word_32(RTC_CNTL_WDTCONFIG0_REG, 0)?;
        // Write protection on
        interface.write_word_32(RTC_CNTL_WDTWPROTECT_REG, 0)?;

        // Write protection off
        interface.write_word_32(RTC_CNTL_SWD_WPROTECT_REG, RTC_CNTL_SWD_WKEY)?;
        // Automatically feed SWD
        let auto_feed_swd =
            interface.read_word_32(RTC_CNTL_SWD_CONF_REG)? | RTC_CNTL_SWD_AUTO_FEED_EN;
        interface.write_word_32(RTC_CNTL_SWD_CONF_REG, auto_feed_swd)?;
        // Write protection on
        interface.write_word_32(RTC_CNTL_SWD_WPROTECT_REG, 0)?;

        tracing::info!("Done disabling watchdogs");
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
            if memory_access_config.default_method(access) != MemoryAccessMethod::SystemBus {
                // External data/instruction bus
                // Loading external memory is slower than the CPU. If we can't access something via the
                // system bus, select the waiting program buffer method.
                memory_access_config.set_region_override(
                    access,
                    0x4000_0000..0x4400_0000,
                    MemoryAccessMethod::WaitingProgramBuffer,
                );
            } else {
                // System bus access to RAM appears broken, and returns garbage
                // values.
                memory_access_config.set_region_override(
                    access,
                    0x4ff0_0000..0x4ffc_0000,
                    MemoryAccessMethod::ProgramBuffer,
                );
                // Also mark uncached access as going through the program buffer
                memory_access_config.set_region_override(
                    access,
                    0x8ff0_0000..0x8ffc_0000,
                    MemoryAccessMethod::ProgramBuffer,
                );
            }
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32P4 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        self.configure_memory_access(interface)?;
        self.disable_wdts(interface)?;

        Ok(())
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        // System reset, ported from OpenOCD.

        interface.halt(timeout)?;
        interface.write_dm_register(Sbcs(0x48000))?;

        // Unstall both CPUs
        // HAL_FORCE_MODIFY_U32_REG_FIELD(PMU.cpu_sw_stall, hpcore*_stall_code, 0xFF);
        interface.write_dm_register(Sbaddress0(0x50115200))?;
        interface.write_dm_register(Sbdata0(0xFFFF0000))?;
        std::thread::sleep(Duration::from_millis(10));

        // Writing LP_SYS_SYS_CTRL_REG causes the System Reset
        // System Reset: resets the whole digital system, including the LP system.
        interface.write_dm_register(Sbaddress0(0x50110008))?;
        // Set (LP_SYS_SYS_SW_RST|LP_SYS_DIG_FIB|LP_SYS_ANA_FIB|LP_SYS_LP_FIB_SEL)
        interface.write_dm_register(Sbdata0(0x1fffc7fa))?;

        // Force on the clock, bypassing the clock gating for all peripherals
        interface.write_dm_register(Sbaddress0(0x500e60b4))?;
        interface.write_dm_register(Sbdata0(0x3FFFF))?;

        std::thread::sleep(Duration::from_millis(10));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.enter_debug_mode()?;

        interface.reset_hart_and_halt(timeout)?;

        // Perform a manual reset of all peripherals
        for reg in &[
            HP_RST_EN0_REG,
            HP_RST_EN1_REG,
            HP_RST_EN2_REG,
            LP_AONCLKRST_LP_RST_EN,
            LP_PERI_RESET_EN,
        ] {
            interface.write_word_32(reg.address, reg.value)?;
        }
        for reg in &[
            HP_RST_EN0_REG,
            HP_RST_EN1_REG,
            HP_RST_EN2_REG,
            LP_AONCLKRST_LP_RST_EN,
            LP_PERI_RESET_EN,
        ] {
            interface.write_word_32(reg.address, 0)?;
        }
        self.on_connect(interface)?;

        Ok(())
    }
}
