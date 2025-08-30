//! Sequence for the ESP32-S2.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::esp::EspFlashSizeDetector;
use crate::{
    MemoryInterface, Session,
    architecture::xtensa::{
        Xtensa,
        communication_interface::{
            MemoryRegionProperties, XtensaCommunicationInterface, XtensaError,
        },
        sequences::XtensaDebugSequence,
        xdm::{self, DebugControlBits, DebugRegisterError},
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
    vendor::espressif::sequences::esp::EspBreakpointHandler,
};

/// The debug sequence implementation for the ESP32-S2.
#[derive(Debug)]
pub struct ESP32S2 {
    inner: EspFlashSizeDetector,
}

impl ESP32S2 {
    const RTC_CNTL_BASE: u64 = 0x3f408000;
    const OPTIONS0: u64 = Self::RTC_CNTL_BASE;
    const CLK_CONF: u64 = Self::RTC_CNTL_BASE + 0x0074;
    const STORE4: u64 = Self::RTC_CNTL_BASE + 0x00BC;
    const STORE5: u64 = Self::RTC_CNTL_BASE + 0x00C0;
    const RTC_CNTL_DIG_PWC_REG: u64 = Self::RTC_CNTL_BASE + 0x8C;
    const SW_CPU_STALL: u64 = Self::RTC_CNTL_BASE + 0x00B8;
    const RTC_WRITE_PROT: u64 = Self::RTC_CNTL_BASE | 0xAC;
    const RTC_WDTCONFIG0: u64 = Self::RTC_CNTL_BASE | 0x94;

    const SWD_BASE: u64 = 0x3f408000;
    const SWD_WRITE_PROT: u64 = Self::SWD_BASE | 0xB4;
    const SWD_CONF: u64 = Self::SWD_BASE | 0xB0;
    const SWD_AUTO_FEED_EN: u32 = 1 << 31;
    const SWD_WRITE_PROT_KEY: u32 = 0x8f1d312a;

    const TIMG0_BASE: u64 = 0x3f41f000;
    const TIMG0_WRITE_PROT: u64 = Self::TIMG0_BASE | 0x64;
    const TIMG0_WDTCONFIG0: u64 = Self::TIMG0_BASE | 0x48;

    const TIMG1_BASE: u64 = 0x3f420000;
    const TIMG1_WRITE_PROT: u64 = Self::TIMG1_BASE | 0x64;
    const TIMG1_WDTCONFIG0: u64 = Self::TIMG1_BASE | 0x48;

    /// Creates a new debug sequence handle for the ESP32-S2.
    pub fn create() -> Arc<dyn XtensaDebugSequence> {
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: 0x3ffce000,
                load_address: 0x4002c400,
                spiflash_peripheral: 0x3f40_2000,
                efuse_get_spiconfig_fn: Some(0x4000e4a0),
                attach_fn: 0x4001_7004,
            },
        })
    }

    fn set_peri_reg_mask(
        &self,
        core: &mut XtensaCommunicationInterface,
        addr: u64,
        mask: u32,
        value: u32,
    ) -> Result<(), crate::Error> {
        let mut reg = core.read_word_32(addr)?;
        reg &= !mask;
        reg |= value;
        core.write_word_32(addr, reg)?;
        Ok(())
    }

    fn set_stall(
        &self,
        stall: bool,
        core: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        const STALL_PROCPU_C1_M: u32 = 0x3F << 26;
        const STALL_PROCPU_C1: u32 = 0x21 << 26;

        const STALL_PROCPU_C0_M: u32 = 0x3 << 2;
        const STALL_PROCPU_C0: u32 = 0x2 << 2;

        self.set_peri_reg_mask(
            core,
            Self::SW_CPU_STALL,
            STALL_PROCPU_C1_M,
            if stall { STALL_PROCPU_C1 } else { 0 },
        )?;
        self.set_peri_reg_mask(
            core,
            Self::OPTIONS0,
            STALL_PROCPU_C0_M,
            if stall { STALL_PROCPU_C0 } else { 0 },
        )?;
        Ok(())
    }

    pub(crate) fn stall(
        &self,
        core: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        self.set_stall(true, core)
    }

    pub(crate) fn unstall(
        &self,
        core: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        self.set_stall(false, core)
    }

    fn disable_wdts(&self, core: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-S2 watchdogs...");

        // disable super wdt
        core.write_word_32(Self::SWD_WRITE_PROT, Self::SWD_WRITE_PROT_KEY)?; // write protection off
        let current = core.read_word_32(Self::SWD_CONF)?;
        core.write_word_32(Self::SWD_CONF, current | Self::SWD_AUTO_FEED_EN)?;
        core.write_word_32(Self::SWD_WRITE_PROT, 0x0)?; // write protection on

        // tg0 wdg
        core.write_word_32(Self::TIMG0_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(Self::TIMG0_WDTCONFIG0, 0x0)?;
        core.write_word_32(Self::TIMG0_WRITE_PROT, 0x0)?; // write protection on

        // tg1 wdg
        core.write_word_32(Self::TIMG1_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(Self::TIMG1_WDTCONFIG0, 0x0)?;
        core.write_word_32(Self::TIMG1_WRITE_PROT, 0x0)?; // write protection on

        // rtc wdg
        core.write_word_32(Self::RTC_WRITE_PROT, 0x50D83AA1)?; // write protection off
        core.write_word_32(Self::RTC_WDTCONFIG0, 0x0)?;
        core.write_word_32(Self::RTC_WRITE_PROT, 0x0)?; // write protection on

        Ok(())
    }
}

impl XtensaDebugSequence for ESP32S2 {
    fn on_connect(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface)?;

        // Internal Data Bus
        interface.core_properties().memory_ranges.insert(
            0x3FF9_E000..0x4000_0000,
            MemoryRegionProperties {
                unaligned_store: true,
                unaligned_load: true,
                fast_memory_access: true,
            },
        );
        // Internal Instruction Bus
        interface.core_properties().memory_ranges.insert(
            0x4000_0000..0x4007_2000,
            MemoryRegionProperties {
                unaligned_store: false,
                unaligned_load: false,
                fast_memory_access: true,
            },
        );
        // External memory busses and peripheral address range uses the default (all false) properties.

        Ok(())
    }

    fn on_halt(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface)
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size(session)
    }

    fn reset_system_and_halt(
        &self,
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        const CLK_CONF_DEF: u32 = 0x1583218;

        const SYS_RESET: u32 = 1 << 31;

        core.reset_and_halt(timeout)?;

        // Set some clock-related RTC registers to the default values
        core.write_word_32(Self::STORE4, 0)?;
        core.write_word_32(Self::STORE5, 0)?;
        core.write_word_32(Self::RTC_CNTL_DIG_PWC_REG, 0)?;
        core.write_word_32(Self::CLK_CONF, CLK_CONF_DEF)?;

        self.stall(core)?;

        core.xdm.debug_control({
            let mut control = DebugControlBits(0);

            control.set_enable_ocd(true);
            control.set_run_stall_in_en(true);

            control
        })?;

        // Reset CPU
        self.set_peri_reg_mask(core, Self::OPTIONS0, SYS_RESET, SYS_RESET)?;

        // Need to manually execute here, because a yet-to-be-flushed write will start the
        // reset process.
        match core.xdm.execute() {
            err @ Err(XtensaError::XdmError(
                xdm::Error::ExecOverrun
                | xdm::Error::InstructionIgnored
                | xdm::Error::Xdm {
                    source: DebugRegisterError::Unexpected(_),
                    ..
                },
            )) => {
                // ignore error
                tracing::debug!("Error ignored: {err:?}");
            }
            other => other?,
        }

        // Wait for reset to happen
        std::thread::sleep(Duration::from_millis(100));
        let start = Instant::now();
        while !core.xdm.read_power_status()?.core_was_reset() {
            if start.elapsed() > timeout {
                return Err(XtensaError::Timeout.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        core.reset_and_halt(timeout)?;

        self.unstall(core)?;

        core.xdm.debug_control({
            let mut control = DebugControlBits(0);

            control.set_enable_ocd(true);

            control
        })?;

        Ok(())
    }

    fn on_unknown_semihosting_command(
        &self,
        interface: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_xtensa_idf_semihosting(interface, details)
    }
}
