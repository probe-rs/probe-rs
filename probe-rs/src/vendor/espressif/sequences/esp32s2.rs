//! Sequence for the ESP32-S2.

use std::{sync::Arc, time::Duration};
use web_time::Instant;

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

    async fn set_peri_reg_mask(
        &self,
        core: &mut XtensaCommunicationInterface<'_>,
        addr: u64,
        mask: u32,
        value: u32,
    ) -> Result<(), crate::Error> {
        let mut reg = core.read_word_32(addr).await?;
        reg &= !mask;
        reg |= value;
        core.write_word_32(addr, reg).await?;
        Ok(())
    }

    async fn set_stall(
        &self,
        stall: bool,
        core: &mut XtensaCommunicationInterface<'_>,
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
        )
        .await?;
        self.set_peri_reg_mask(
            core,
            Self::OPTIONS0,
            STALL_PROCPU_C0_M,
            if stall { STALL_PROCPU_C0 } else { 0 },
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn stall(
        &self,
        core: &mut XtensaCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        self.set_stall(true, core).await
    }

    pub(crate) async fn unstall(
        &self,
        core: &mut XtensaCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        self.set_stall(false, core).await
    }

    async fn disable_wdts(
        &self,
        core: &mut XtensaCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32-S2 watchdogs...");

        // disable super wdt
        core.write_word_32(Self::SWD_WRITE_PROT, Self::SWD_WRITE_PROT_KEY)
            .await?; // write protection off
        let current = core.read_word_32(Self::SWD_CONF).await?;
        core.write_word_32(Self::SWD_CONF, current | Self::SWD_AUTO_FEED_EN)
            .await?;
        core.write_word_32(Self::SWD_WRITE_PROT, 0x0).await?; // write protection on

        // tg0 wdg
        core.write_word_32(Self::TIMG0_WRITE_PROT, 0x50D83AA1)
            .await?; // write protection off
        core.write_word_32(Self::TIMG0_WDTCONFIG0, 0x0).await?;
        core.write_word_32(Self::TIMG0_WRITE_PROT, 0x0).await?; // write protection on

        // tg1 wdg
        core.write_word_32(Self::TIMG1_WRITE_PROT, 0x50D83AA1)
            .await?; // write protection off
        core.write_word_32(Self::TIMG1_WDTCONFIG0, 0x0).await?;
        core.write_word_32(Self::TIMG1_WRITE_PROT, 0x0).await?; // write protection on

        // rtc wdg
        core.write_word_32(Self::RTC_WRITE_PROT, 0x50D83AA1).await?; // write protection off
        core.write_word_32(Self::RTC_WDTCONFIG0, 0x0).await?;
        core.write_word_32(Self::RTC_WRITE_PROT, 0x0).await?; // write protection on

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl XtensaDebugSequence for ESP32S2 {
    async fn on_connect(
        &self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        // Peripherals
        interface.core_properties().memory_ranges.insert(
            0x3F40_0000..0x3F50_0000,
            MemoryRegionProperties {
                unaligned_store: false,
                unaligned_load: false,
                fast_memory_access: true,
            },
        );
        // Data
        interface.core_properties().memory_ranges.insert(
            0x3FF9_E000..0x4000_0000,
            MemoryRegionProperties {
                unaligned_store: true,
                unaligned_load: true,
                fast_memory_access: true,
            },
        );
        // Instruction
        interface.core_properties().memory_ranges.insert(
            0x4000_0000..0x4007_2000,
            MemoryRegionProperties {
                unaligned_store: false,
                unaligned_load: false,
                fast_memory_access: true,
            },
        );

        self.disable_wdts(interface).await
    }

    async fn on_halt(&self, interface: &mut XtensaCommunicationInterface<'_>) -> Result<(), crate::Error> {
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
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        const CLK_CONF_DEF: u32 = 0x1583218;

        const SYS_RESET: u32 = 1 << 31;

        core.reset_and_halt(timeout).await?;

        // Set some clock-related RTC registers to the default values
        core.write_word_32(Self::STORE4, 0).await?;
        core.write_word_32(Self::STORE5, 0).await?;
        core.write_word_32(Self::RTC_CNTL_DIG_PWC_REG, 0).await?;
        core.write_word_32(Self::CLK_CONF, CLK_CONF_DEF).await?;

        self.stall(core).await?;

        core.xdm
            .debug_control({
                let mut control = DebugControlBits(0);

                control.set_enable_ocd(true);
                control.set_run_stall_in_en(true);

                control
            })
            .await?;

        // Reset CPU
        self.set_peri_reg_mask(core, Self::OPTIONS0, SYS_RESET, SYS_RESET)
            .await?;

        // Need to manually execute here, because a yet-to-be-flushed write will start the
        // reset process.
        match core.xdm.execute().await {
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
        while !core.xdm.read_power_status().await?.core_was_reset() {
            if start.elapsed() > timeout {
                return Err(XtensaError::Timeout.into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        core.reset_and_halt(timeout).await?;

        self.unstall(core).await?;

        core.xdm
            .debug_control({
                let mut control = DebugControlBits(0);

                control.set_enable_ocd(true);

                control
            })
            .await?;

        Ok(())
    }

    async fn on_unknown_semihosting_command(
        &self,
        interface: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_xtensa_idf_semihosting(interface, details).await
    }
}
