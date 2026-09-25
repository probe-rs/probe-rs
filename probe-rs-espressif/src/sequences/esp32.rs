//! Sequence for the ESP32.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::sequences::esp::EspBreakpointHandler;
use probe_rs::{
    Error, MemoryInterface,
    architecture::xtensa::{
        Xtensa,
        communication_interface::{
            MemoryRegionProperties, ProgramCounter, XtensaCommunicationInterface, XtensaError,
        },
        sequences::XtensaDebugSequence,
        xdm,
    },
    semihosting::{SemihostingCommand, UnknownCommandDetails},
};

// A program that does the system reset and then loops,
// because system reset seems to disable JTAG.
// Taken from https://github.com/espressif/openocd-esp32/tree/de4a2ae782c33a603e134f3376ecad4e3a8a545d/contrib/loaders/reset/espressif/esp32
// TODO: rework this into some readable code
const RESET_STUB: [u8; 210] = [
    0x06, 0x1e, 0x00, 0x00, 0x06, 0x14, 0x00, 0x00, 0x34, 0x80, 0xf4, 0x3f, 0xb0, 0x80, 0xf4, 0x3f,
    0xb4, 0x80, 0xf4, 0x3f, 0x70, 0x80, 0xf4, 0x3f, 0x10, 0x22, 0x00, 0x00, 0x00, 0x20, 0x49, 0x9c,
    0x00, 0x80, 0xf4, 0x3f, 0xa1, 0x3a, 0xd8, 0x50, 0xa4, 0x80, 0xf4, 0x3f, 0x64, 0xf0, 0xf5, 0x3f,
    0x64, 0x00, 0xf6, 0x3f, 0x8c, 0x80, 0xf4, 0x3f, 0x48, 0xf0, 0xf5, 0x3f, 0x48, 0x00, 0xf6, 0x3f,
    0xfc, 0xa1, 0xf5, 0x3f, 0x38, 0x00, 0xf0, 0x3f, 0x30, 0x00, 0xf0, 0x3f, 0x2c, 0x00, 0xf0, 0x3f,
    0x34, 0x80, 0xf4, 0x3f, 0x00, 0x30, 0x00, 0x00, 0x50, 0x55, 0x30, 0x41, 0xeb, 0xff, 0x59, 0x04,
    0x41, 0xeb, 0xff, 0x59, 0x04, 0x41, 0xea, 0xff, 0x59, 0x04, 0x41, 0xea, 0xff, 0x31, 0xea, 0xff,
    0x39, 0x04, 0x31, 0xea, 0xff, 0x41, 0xea, 0xff, 0x39, 0x04, 0x00, 0x00, 0x60, 0xeb, 0x03, 0x60,
    0x61, 0x04, 0x56, 0x66, 0x04, 0x50, 0x55, 0x30, 0x31, 0xe7, 0xff, 0x41, 0xe7, 0xff, 0x39, 0x04,
    0x41, 0xe7, 0xff, 0x39, 0x04, 0x41, 0xe6, 0xff, 0x39, 0x04, 0x41, 0xe6, 0xff, 0x59, 0x04, 0x41,
    0xe6, 0xff, 0x59, 0x04, 0x41, 0xe6, 0xff, 0x59, 0x04, 0x41, 0xe5, 0xff, 0x59, 0x04, 0x41, 0xe5,
    0xff, 0x59, 0x04, 0x41, 0xe5, 0xff, 0x0c, 0x13, 0x39, 0x04, 0x41, 0xe4, 0xff, 0x0c, 0x13, 0x39,
    0x04, 0x59, 0x04, 0x41, 0xe3, 0xff, 0x31, 0xe3, 0xff, 0x32, 0x64, 0x00, 0x00, 0x70, 0x00, 0x46,
    0xfe, 0xff,
];

/// Why a reset attempt failed.
enum ResetFailure {
    /// The debug connection still works, so another attempt can run.
    Retry(crate::Error),

    /// Another attempt cannot succeed. Either the chip no longer answers, or it can only boot
    /// from the stub in RTC slow memory.
    GiveUp(crate::Error),
}

/// The debug sequence implementation for the ESP32.
#[derive(Debug)]
pub struct ESP32 {}

impl ESP32 {
    const RTC_SLOW_MEM: u64 = 0x5000_0000;
    const RTC_CNTL_RESET_STATE_REG: u64 = 0x3ff48034;
    const RTC_CNTL_RESET_STATE_DEF: u32 = 0x3000;

    const RTC_CNTL_DIG_PWC_REG: u64 = 0x3FF48084;
    const DG_WRAP_PD_EN: u32 = 1 << 31;
    const DG_WRAP_FORCE_PU: u32 = 1 << 20;
    const DG_WRAP_FORCE_PD: u32 = 1 << 19;

    /// Creates a new debug sequence handle for the ESP32.
    pub fn create() -> Arc<dyn XtensaDebugSequence> {
        tracing::warn!(
            "Be careful not to reset your ESP32 while connected to the debugger! Depending on the specific device, this may render it temporarily inoperable or permanently damage it."
        );
        Arc::new(Self {})
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

    fn configure_memory_access(
        &self,
        interface: &mut XtensaCommunicationInterface<'_>,
    ) -> Result<(), crate::Error> {
        // Internal Data Bus
        interface.core_properties().memory_ranges.insert(
            0x3FF8_0000..0x4000_0000,
            MemoryRegionProperties {
                unaligned_store: true,
                unaligned_load: true,
                fast_memory_access: true,
            },
        );
        // Internal Instruction Bus
        interface.core_properties().memory_ranges.insert(
            0x4000_0000..0x400C_2000,
            MemoryRegionProperties {
                unaligned_store: false,
                unaligned_load: false,
                fast_memory_access: true,
            },
        );
        // External memory busses and peripheral address range uses the default (all false) properties.

        Ok(())
    }
}

impl XtensaDebugSequence for ESP32 {
    fn on_connect(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), Error> {
        self.configure_memory_access(interface)?;
        self.disable_wdts(interface)?;

        Ok(())
    }

    fn on_halt(&self, interface: &mut XtensaCommunicationInterface) -> Result<(), Error> {
        self.disable_wdts(interface)
    }

    fn reset_system_and_halt(
        &self,
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        const ATTEMPTS: u32 = 3;

        // A failed attempt leaves the stub in RTC slow memory, so a later attempt would back up
        // the stub instead of the original contents.
        let mut backup = None;

        let mut last_error = None;
        for attempt in 1..=ATTEMPTS {
            match self.try_reset_system_and_halt(core, timeout, &mut backup) {
                Ok(()) => return Ok(()),
                Err(ResetFailure::GiveUp(error)) => return Err(error),
                Err(ResetFailure::Retry(error)) => {
                    tracing::warn!("Reset attempt {attempt} of {ATTEMPTS} failed: {error}");
                    last_error = Some(error);
                }
            }
        }

        Err(last_error.expect("at least one attempt has run"))
    }

    fn on_unknown_semihosting_command(
        &self,
        interface: &mut Xtensa,
        details: UnknownCommandDetails,
    ) -> Result<Option<SemihostingCommand>, crate::Error> {
        EspBreakpointHandler::handle_xtensa_idf_semihosting(interface, details)
    }
}

impl ESP32 {
    /// Resets the chip by running a stub from RTC slow memory, and halts the CPU afterwards.
    ///
    /// The stub is needed because a system reset disables JTAG on rev. 3 silicon: the PRO CPU
    /// has to re-enable it, disable the watchdogs and restore the reset vector selection after
    /// the reset. If this sequence gives up before the stub has done so, the chip stays
    /// unreachable until the next power-on reset, so both pieces of state this function changes
    /// are put back even when the reset fails.
    fn try_reset_system_and_halt(
        &self,
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
        backup: &mut Option<Vec<u8>>,
    ) -> Result<(), ResetFailure> {
        {
            let _span = tracing::debug_span!("Resetting core").entered();
            // The previous attempt may have left the debug module reset, in which case it only
            // answers again after the JTAG link is set up from scratch.
            core.enter_debug_mode()
                .map_err(|error| ResetFailure::GiveUp(error.into()))?;
            core.reset_and_halt(timeout)
                .map_err(|error| ResetFailure::GiveUp(error.into()))?;
            self.disable_wdts(core).map_err(ResetFailure::Retry)?;
        }

        if backup.is_none() {
            let _span = tracing::debug_span!("Backing up RTC_SLOW").entered();
            let mut ram_value = vec![0; RESET_STUB.len()];
            core.read(Self::RTC_SLOW_MEM, &mut ram_value)
                .map_err(ResetFailure::Retry)?;
            *backup = Some(ram_value);
        }

        let result = self.run_reset_stub(core, timeout);

        let boots_from_rom = match &result {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!("Reset failed, cleaning up: {error}");

                // The stub resets the debug module along with the rest of the system, so the
                // JTAG link has to be set up again before the core answers.
                if let Err(error) = core.enter_debug_mode() {
                    tracing::warn!("Failed to enter debug mode after the failed reset: {error}");
                }

                // The CPU may still be running the stub, or code from wherever the reset
                // vector points at. Take control before changing memory.
                if let Err(error) = core.reset_and_halt(timeout) {
                    tracing::warn!("Failed to halt the core after the failed reset: {error}");
                }

                // Point the reset vector back at ROM. Left cleared, the next system reset
                // makes both CPUs execute the contents of RTC slow memory.
                match core.write_word_32(
                    Self::RTC_CNTL_RESET_STATE_REG,
                    Self::RTC_CNTL_RESET_STATE_DEF,
                ) {
                    Ok(()) => true,
                    Err(error) => {
                        tracing::warn!("Failed to restore the reset vector selection: {error}");
                        false
                    }
                }
            }
        };

        if !boots_from_rom {
            // The CPUs still boot from RTC slow memory, so the stub has to stay there: it
            // re-enables JTAG and points the reset vector back at ROM, which lets the next
            // reset recover the chip. Putting the original contents back instead would make
            // the CPUs execute them, and only a power-on reset could recover from that.
            tracing::warn!(
                "Keeping the reset stub in RTC slow memory to recover on the next reset"
            );
            // The chip does not answer any more, so a further attempt cannot reach it either.
            return Err(ResetFailure::GiveUp(
                result.expect_err("the reset vector is only restored after a failure"),
            ));
        }

        let restore = {
            let _span = tracing::debug_span!("Restore RAM contents").entered();
            core.write(
                Self::RTC_SLOW_MEM,
                backup.as_deref().expect("the backup has just been taken"),
            )
        };

        result.and(restore).map_err(ResetFailure::Retry)?;

        tracing::info!("Reset complete");

        Ok(())
    }

    fn run_reset_stub(
        &self,
        core: &mut XtensaCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        {
            let _span = tracing::debug_span!("Downloading code").entered();
            core.write(Self::RTC_SLOW_MEM, &RESET_STUB)?;
            // Offset 4 is the entry point that resets the chip. Offset 0 is where the CPUs
            // start after that reset.
            core.write_register(ProgramCounter(0x5000_0004))?;
        }

        {
            let _span =
                tracing::debug_span!("Make sure the ready value is not what we expect").entered();
            let reset_state = core.read_word_32(Self::RTC_CNTL_RESET_STATE_REG)?;
            let new_state = reset_state & !Self::RTC_CNTL_RESET_STATE_DEF;
            core.write_word_32(Self::RTC_CNTL_RESET_STATE_REG, new_state)?;
        }

        // Firmware may have left the digital core set to power down; it has to be on for the
        // stub to run after the reset.
        let dig_pwc = core.read_word_32(Self::RTC_CNTL_DIG_PWC_REG)?;
        core.write_word_32(
            Self::RTC_CNTL_DIG_PWC_REG,
            (dig_pwc & !(Self::DG_WRAP_PD_EN | Self::DG_WRAP_FORCE_PD)) | Self::DG_WRAP_FORCE_PU,
        )?;

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

        // The stub finishes the reset in microseconds, so a read that fails here has hit the
        // reset itself. Give the chip a few tries before treating the link as lost.
        const ALLOWED_CONSECUTIVE_ERRORS: u32 = 5;

        let start = Instant::now();
        let mut errors = 0;
        tracing::debug!("Waiting for program to complete");
        loop {
            // RTC_CNTL_RESET_STATE_REG is the last one to be set,
            // so if it's set, the program has completed.
            match core.read_word_32(Self::RTC_CNTL_RESET_STATE_REG) {
                Ok(reset_state) => {
                    errors = 0;
                    tracing::debug!("Reset status register: {:#010x}", reset_state);
                    if reset_state & Self::RTC_CNTL_RESET_STATE_DEF
                        == Self::RTC_CNTL_RESET_STATE_DEF
                    {
                        break;
                    }
                }
                Err(error) => {
                    errors += 1;
                    if errors > ALLOWED_CONSECUTIVE_ERRORS {
                        return Err(error);
                    }
                    tracing::debug!("Ignoring read error {errors}: {error}");
                }
            }

            if start.elapsed() >= timeout {
                return Err(XtensaError::Timeout.into());
            }

            // Each read halts and resumes the CPU that runs the stub. Leave it alone between
            // the polls.
            std::thread::sleep(Duration::from_millis(1));
        }

        core.reset_and_halt(timeout)?;
        self.on_connect(core)?;

        Ok(())
    }
}
