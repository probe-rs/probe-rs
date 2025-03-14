//! Sequence for the ESP32.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::esp::EspFlashSizeDetector;
use crate::{
    MemoryInterface, Session,
    architecture::xtensa::{
        communication_interface::{ProgramCounter, XtensaCommunicationInterface, XtensaError},
        sequences::XtensaDebugSequence,
        xdm,
    },
};

/// The debug sequence implementation for the ESP32.
#[derive(Debug)]
pub struct ESP32 {
    inner: EspFlashSizeDetector,
}

impl ESP32 {
    /// Creates a new debug sequence handle for the ESP32.
    pub fn create() -> Arc<dyn XtensaDebugSequence> {
        tracing::warn!(
            "Be careful not to reset your ESP32 while connected to the debugger! Depending on the specific device, this may render it temporarily inoperable or permanently damage it."
        );
        Arc::new(Self {
            inner: EspFlashSizeDetector {
                stack_pointer: 0x3ffd0000,
                load_address: 0x4009_0000,
                spiflash_peripheral: 0x3ff4_2000,
                efuse_get_spiconfig_fn: Some(0x40008658),
                attach_fn: 0x4006_2a6c,
            },
        })
    }

    fn disable_wdts(
        &self,
        interface: &mut XtensaCommunicationInterface,
    ) -> Result<(), crate::Error> {
        tracing::info!("Disabling ESP32 watchdogs...");

        // tg0 wdg
        const TIMG0_BASE: u64 = 0x3ff5f000;
        const TIMG0_WRITE_PROT: u64 = TIMG0_BASE | 0x64;
        const TIMG0_WDTCONFIG0: u64 = TIMG0_BASE | 0x48;
        interface.write_word_32(TIMG0_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(TIMG0_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG0_WRITE_PROT, 0x0)?; // write protection on

        // tg1 wdg
        const TIMG1_BASE: u64 = 0x3ff60000;
        const TIMG1_WRITE_PROT: u64 = TIMG1_BASE | 0x64;
        const TIMG1_WDTCONFIG0: u64 = TIMG1_BASE | 0x48;
        interface.write_word_32(TIMG1_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(TIMG1_WDTCONFIG0, 0x0)?;
        interface.write_word_32(TIMG1_WRITE_PROT, 0x0)?; // write protection on

        // rtc wdg
        const RTC_CNTL_BASE: u64 = 0x3ff48000;
        const RTC_WRITE_PROT: u64 = RTC_CNTL_BASE | 0xa4;
        const RTC_WDTCONFIG0: u64 = RTC_CNTL_BASE | 0x8c;
        interface.write_word_32(RTC_WRITE_PROT, 0x50D83AA1)?; // write protection off
        interface.write_word_32(RTC_WDTCONFIG0, 0x0)?;
        interface.write_word_32(RTC_WRITE_PROT, 0x0)?; // write protection on

        Ok(())
    }
}

impl XtensaDebugSequence for ESP32 {
    fn on_connect(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        // Peripheral address range
        interface.add_slow_memory_access_range(0x3FF0_0000..0x3FF8_0000);

        self.disable_wdts(interface)
    }

    fn on_halt(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), crate::Error> {
        self.disable_wdts(interface)
    }

    fn detect_flash_size(&self, session: &mut Session) -> Result<Option<usize>, crate::Error> {
        self.inner.detect_flash_size_esp32(session)
    }

    fn reset_system_and_halt(
        &self,
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        const RTC_CNTL_BASE: u64 = 0x3ff48000;

        const RTC_CNTL_RESET_STATE_REG: u64 = RTC_CNTL_BASE + 0x34;
        const RTC_CNTL_RESET_STATE_DEF: u32 = 0x3000;

        {
            let _span = tracing::debug_span!("Halting core").entered();
            if !core.core_halted()? {
                core.halt(timeout)?;
            }
        }

        // A program that does the system reset and then loops,
        // because system reset seems to disable JTAG.
        // Taken from https://github.com/espressif/openocd-esp32/tree/de4a2ae782c33a603e134f3376ecad4e3a8a545d/contrib/loaders/reset/espressif/esp32
        // TODO: rework this into some readable code
        let instructions = [
            0x06, 0x1e, 0x00, 0x00, 0x06, 0x14, 0x00, 0x00, 0x34, 0x80, 0xf4, 0x3f, 0xb0, 0x80,
            0xf4, 0x3f, 0xb4, 0x80, 0xf4, 0x3f, 0x70, 0x80, 0xf4, 0x3f, 0x10, 0x22, 0x00, 0x00,
            0x00, 0x20, 0x49, 0x9c, 0x00, 0x80, 0xf4, 0x3f, 0xa1, 0x3a, 0xd8, 0x50, 0xa4, 0x80,
            0xf4, 0x3f, 0x64, 0xf0, 0xf5, 0x3f, 0x64, 0x00, 0xf6, 0x3f, 0x8c, 0x80, 0xf4, 0x3f,
            0x48, 0xf0, 0xf5, 0x3f, 0x48, 0x00, 0xf6, 0x3f, 0xfc, 0xa1, 0xf5, 0x3f, 0x38, 0x00,
            0xf0, 0x3f, 0x30, 0x00, 0xf0, 0x3f, 0x2c, 0x00, 0xf0, 0x3f, 0x34, 0x80, 0xf4, 0x3f,
            0x00, 0x30, 0x00, 0x00, 0x50, 0x55, 0x30, 0x41, 0xeb, 0xff, 0x59, 0x04, 0x41, 0xeb,
            0xff, 0x59, 0x04, 0x41, 0xea, 0xff, 0x59, 0x04, 0x41, 0xea, 0xff, 0x31, 0xea, 0xff,
            0x39, 0x04, 0x31, 0xea, 0xff, 0x41, 0xea, 0xff, 0x39, 0x04, 0x00, 0x00, 0x60, 0xeb,
            0x03, 0x60, 0x61, 0x04, 0x56, 0x66, 0x04, 0x50, 0x55, 0x30, 0x31, 0xe7, 0xff, 0x41,
            0xe7, 0xff, 0x39, 0x04, 0x41, 0xe7, 0xff, 0x39, 0x04, 0x41, 0xe6, 0xff, 0x39, 0x04,
            0x41, 0xe6, 0xff, 0x59, 0x04, 0x41, 0xe6, 0xff, 0x59, 0x04, 0x41, 0xe6, 0xff, 0x59,
            0x04, 0x41, 0xe5, 0xff, 0x59, 0x04, 0x41, 0xe5, 0xff, 0x59, 0x04, 0x41, 0xe5, 0xff,
            0x0c, 0x13, 0x39, 0x04, 0x41, 0xe4, 0xff, 0x0c, 0x13, 0x39, 0x04, 0x59, 0x04, 0x41,
            0xe3, 0xff, 0x31, 0xe3, 0xff, 0x32, 0x64, 0x00, 0x00, 0x70, 0x00, 0x46, 0xfe, 0xff,
        ];

        let mut ram_value = vec![0; std::mem::size_of_val(&instructions)];

        {
            let _span = tracing::debug_span!("Backing up RTC_SLOW").entered();
            core.read(0x5000_0000, &mut ram_value)?;
        }

        {
            let _span = tracing::debug_span!("Downloading code").entered();
            core.write(0x5000_0000, &instructions)?;
            core.write_register(ProgramCounter(0x5000_0004))?;
        }

        {
            let _span =
                tracing::debug_span!("Make sure the ready value is not what we expect").entered();
            let reset_state = core.read_word_32(RTC_CNTL_RESET_STATE_REG)?;
            let new_state = reset_state & !RTC_CNTL_RESET_STATE_DEF;
            core.write_word_32(RTC_CNTL_RESET_STATE_REG, new_state)?;
        }

        match core.resume_core() {
            err @ Err(XtensaError::XdmError(
                xdm::Error::ExecOverrun | xdm::Error::InstructionIgnored,
            )) => {
                // ignore error
                tracing::debug!("Error ignored: {err:?}");
            }
            other => other?,
        }

        std::thread::sleep(Duration::from_millis(100));

        core.enter_debug_mode()?;

        let start = Instant::now();
        tracing::debug!("Waiting for program to complete");
        loop {
            // RTC_CNTL_RESET_STATE_REG is the last one to be set,
            // so if it's set, the program has completed.
            let reset_state = core.read_word_32(RTC_CNTL_RESET_STATE_REG)?;

            tracing::debug!("Reset status register: {:#010x}", reset_state);
            if reset_state & RTC_CNTL_RESET_STATE_DEF == RTC_CNTL_RESET_STATE_DEF {
                break;
            }

            if start.elapsed() >= timeout {
                return Err(crate::Error::Timeout);
            }
        }

        core.reset_and_halt(timeout)?;

        {
            let _span = tracing::debug_span!("Restore RAM contents").entered();
            core.write(0x5000_0000, &ram_value)?;
        }

        tracing::info!("Reset complete");

        Ok(())
    }
}
