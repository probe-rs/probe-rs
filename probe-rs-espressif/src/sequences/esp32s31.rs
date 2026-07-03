//! Sequences for the ESP32-S31.

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

// Timer Group 0 WDT registers
const DR_REG_TIMG0_BASE: u64 = 0x2058_0000;
const TIMG0_WDTCONFIG0_REG: u64 = DR_REG_TIMG0_BASE + 0x48;
const TIMG0_WDTWPROTECT_REG: u64 = DR_REG_TIMG0_BASE + 0x64;
const TIMG0_INT_CLR_TIMERS_REG: u64 = DR_REG_TIMG0_BASE + 0x7c;

// Timer Group 1 WDT registers
const DR_REG_TIMG1_BASE: u64 = 0x2058_1000;
const TIMG1_WDTCONFIG0_REG: u64 = DR_REG_TIMG1_BASE + 0x48;
const TIMG1_WDTWPROTECT_REG: u64 = DR_REG_TIMG1_BASE + 0x64;
const TIMG1_INT_CLR_TIMERS_REG: u64 = DR_REG_TIMG1_BASE + 0x7c;

const TIMG_WDT_WKEY: u32 = 0x50D8_3AA1;
const TIMG_INT_CLR_WDG_INT: u32 = 0x2;

// LP WDT registers (base at 0x20801000)
const DR_REG_LP_WDT_BASE: u64 = 0x2080_1000;
const LP_WDT_WDTCONFIG0_REG: u64 = DR_REG_LP_WDT_BASE;
const LP_WDT_WDTWPROTECT_REG: u64 = DR_REG_LP_WDT_BASE + 0x18;
const LP_WDT_SWD_CONF_REG: u64 = DR_REG_LP_WDT_BASE + 0x1c;
const LP_WDT_SWD_WPROTECT_REG: u64 = DR_REG_LP_WDT_BASE + 0x20;
const LP_WDT_INT_CLR_REG: u64 = DR_REG_LP_WDT_BASE + 0x30;
const LP_WDT_WKEY: u32 = 0x50D8_3AA1;
/// Bit 30: disable the super WDT (SWD_DISABLE_BIT)
const LP_WDT_SWD_DISABLE: u32 = 1 << 30;
const LP_WDT_INT_CLR_ALL: u32 = 0xC000_0000;

// PMU CPU stall register (to unstall both CPUs before reset)
const PMU_CPU_STALL_SW_REG: u32 = 0x2070_41EC;
/// Writing 0xFFFF0000 clears stall codes for both HP cores
const PMU_CPU_UNSTALL_VALUE: u32 = 0xFFFF_0000;

// LP system reset register
const LP_SYSTEM_REG_SYS_CTRL_REG: u32 = 0x2070_0008;
/// Bit 1: trigger a full digital-system reset
const LP_SYSTEM_REG_SYS_SW_RST: u32 = 1 << 1;

// HP_SYS_CLKRST CORE1 control register (enables clocks so core1 can be examined)
const HP_SYS_CLKRST_HPCORE1_CTRL0_REG: u32 = 0x2058_7020;
const HPCORE1_CLK_ENABLE: u32 = 0x3;

/// The debug sequence implementation for the ESP32-S31.
#[derive(Debug)]
pub struct ESP32S31 {}

impl ESP32S31 {
    /// Creates a new debug sequence handle for the ESP32-S31.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {})
    }

    fn disable_wdts(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        tracing::info!("Disabling ESP32-S31 watchdogs...");

        // TG0 WDT: write-protect off, disable, clear interrupt
        interface.write_word_32(TIMG0_WDTWPROTECT_REG, TIMG_WDT_WKEY)?;
        interface.write_word_32(TIMG0_WDTCONFIG0_REG, 0x0)?;
        interface.write_word_32(TIMG0_INT_CLR_TIMERS_REG, TIMG_INT_CLR_WDG_INT)?;

        // TG1 WDT: write-protect off, disable, clear interrupt
        interface.write_word_32(TIMG1_WDTWPROTECT_REG, TIMG_WDT_WKEY)?;
        interface.write_word_32(TIMG1_WDTCONFIG0_REG, 0x0)?;
        interface.write_word_32(TIMG1_INT_CLR_TIMERS_REG, TIMG_INT_CLR_WDG_INT)?;

        // LP WDT RTC: write-protect off, disable
        interface.write_word_32(LP_WDT_WDTWPROTECT_REG, LP_WDT_WKEY)?;
        interface.write_word_32(LP_WDT_WDTCONFIG0_REG, 0x0)?;

        // LP WDT SWD: write-protect off, disable by setting SWD_DISABLE bit
        interface.write_word_32(LP_WDT_SWD_WPROTECT_REG, LP_WDT_WKEY)?;
        interface.write_word_32(LP_WDT_SWD_CONF_REG, LP_WDT_SWD_DISABLE)?;

        // Clear LP WDT (RTC + SWD) interrupt status
        interface.write_word_32(LP_WDT_INT_CLR_REG, LP_WDT_INT_CLR_ALL)?;

        tracing::info!("Done disabling ESP32-S31 watchdogs");
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
            // Flash mapped at XIP addresses (IROM + DROM): use waiting program buffer
            memory_access_config.set_region_override(
                access,
                0x4000_0000..0x4800_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );

            // Peripheral/LP register space: use waiting program buffer
            memory_access_config.set_region_override(
                access,
                0x2000_0000..0x2100_0000,
                MemoryAccessMethod::WaitingProgramBuffer,
            );

            // Internal HP DRAM: use program buffer to avoid bus access issues
            memory_access_config.set_region_override(
                access,
                0x2F00_0000..0x2F08_0000,
                MemoryAccessMethod::ProgramBuffer,
            );
        }

        Ok(())
    }
}

impl RiscvDebugSequence for ESP32S31 {
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), Error> {
        self.configure_memory_access(interface)?;
        self.disable_wdts(interface)?;

        Ok(())
    }

    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), Error> {
        // System reset ported from OpenOCD esp32s31_soc_reset.

        interface.halt(timeout)?;

        // Configure system bus for 32-bit access
        interface.write_dm_register(Sbcs(0x40000))?;

        // Unstall both HP CPU cores via PMU_CPU_STALL_SW_REG
        interface.write_dm_register(Sbaddress0(PMU_CPU_STALL_SW_REG))?;
        interface.write_dm_register(Sbdata0(PMU_CPU_UNSTALL_VALUE))?;
        std::thread::sleep(Duration::from_millis(10));

        // Trigger a full digital-system reset via LP_SYSTEM_REG_SYS_CTRL_REG bit 1.
        // Must read-modify-write to preserve other bits.
        // Configure SB for read-on-addr + 32-bit access
        interface.write_dm_register(Sbcs(0x140000))?;
        interface.write_dm_register(Sbaddress0(LP_SYSTEM_REG_SYS_CTRL_REG))?;
        let reg_val = interface.read_dm_register::<Sbdata0>()?.0;
        // Switch back to write mode and trigger reset
        interface.write_dm_register(Sbcs(0x40000))?;
        interface.write_dm_register(Sbaddress0(LP_SYSTEM_REG_SYS_CTRL_REG))?;
        interface.write_dm_register(Sbdata0(reg_val | LP_SYSTEM_REG_SYS_SW_RST))?;

        // Enable CORE1 clocks so it can be examined post-reset
        interface.write_dm_register(Sbaddress0(HP_SYS_CLKRST_HPCORE1_CTRL0_REG))?;
        interface.write_dm_register(Sbdata0(HPCORE1_CLK_ENABLE))?;

        std::thread::sleep(Duration::from_millis(10));

        let mut dmcontrol = Dmcontrol(0);
        dmcontrol.set_dmactive(true);
        dmcontrol.set_ackhavereset(true);
        interface.write_dm_register(dmcontrol)?;

        interface.enter_debug_mode()?;
        self.on_connect(interface)?;

        const ECC_MEM_LP_CTRL: u64 = 0x2058_6218;
        let val = interface.read_word_32(ECC_MEM_LP_CTRL)?;
        // clear HP_SYSTEM_ECC_MEM_LP_EN (bit 2), set HP_SYSTEM_ECC_MEM_LP_FORCE_CTRL (bit 3)
        interface.write_word_32(ECC_MEM_LP_CTRL, (val & !(1 << 2)) | (1 << 3))?;

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
