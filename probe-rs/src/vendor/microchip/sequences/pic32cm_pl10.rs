//! Sequences for the PIC32CM PL10 target family.
//!
//! These devices run a Boot ROM after every reset. When a debugger is present, the Boot ROM
//! talks to it through the DSU Boot ROM Communication Channels (BCC). The debugger resets the
//! device with a cold-plugging sequence, which holds the CPU in CPU Reset Extension (CRSTEXT0),
//! releases the CPU, and then tells the Boot ROM to enter Park mode, where the core can be
//! halted. The Boot ROM then waits for the Boot ROM Phase Extension (BREXT0) to be released
//! before it starts the application.
//!
//! The sequences here follow the MPLAB programming scripts in the Microchip PIC32CM PL10 device
//! family pack, which differ from the CMSIS sequences in the same pack.

use crate::{
    MemoryMappedRegister, Permissions,
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress, Pins,
        communication_interface::SwdSequence,
        core::{
            armv6m::{BpCompx, BpCtrl, Dhcsr},
            cortex_m,
            registers::cortex_m::{CORTEX_M_CORE_REGISTERS, PC},
        },
        memory::ArmMemoryInterface,
        sequences::{ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence},
        traits::DebugPortWire,
    },
    session::MissingPermissions,
    vendor::microchip::sequences::atsam::SwdSequenceShim,
};
use probe_rs_target::CoreType;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

/// DSU Status A register.
const DSU_STATUSA: u64 = 0x4100_2104;
/// DSU Status B register.
const DSU_STATUSB: u64 = 0x4100_2108;
/// DSU Boot ROM Communication Channel 0, written by the debugger.
const DSU_BCC0: u64 = 0x4100_2110;
/// DSU Boot ROM Communication Channel 1, written by the Boot ROM.
const DSU_BCC1: u64 = 0x4100_2114;
/// DSU Debugger Access Level register.
const DSU_DAL: u64 = 0x4100_2124;

/// DSU.STATUSA: CPU0 Reset Extension (CRSTEXT0).
const STATUSA_CRSTEXT0: u32 = 1 << 8;
/// DSU.STATUSA: Boot ROM 0 Phase Extension (BREXT0).
const STATUSA_BREXT0: u32 = 1 << 16;
/// DSU.STATUSB: BCC0 holds a value that the Boot ROM has not read yet.
const STATUSB_BCCD0: u32 = 1 << 0;
/// DSU.STATUSB: BCC1 holds a value that the debugger has not read yet.
const STATUSB_BCCD1: u32 = 1 << 1;

/// Debugger commands to the Boot ROM.
const CMD_EXIT: u32 = 0x4442_47AA;
const CMD_IMODE: u32 = 0x4442_4755;
const CMD_CE_ALL: u32 = 0x4442_47E3;

/// Boot ROM statuses.
const STATUS_INITCHECK_OK: u32 = 0x3;
const STATUS_BOOTOK: u32 = 0x4;
const STATUS_CMD_VALID: u32 = 0x5;
const STATUS_ARG_INVALID: u32 = 0x6;
const STATUS_OK: u32 = 0x9;
const STATUS_CHALLENGE: u32 = 0xB;
/// Boot ROM statuses with these upper bits report an error, with the code in the low byte.
const STATUS_ERROR_MASK: u32 = 0xFFFF_0000;
const STATUS_ERROR: u32 = 0xEEEE_0000;

/// NVMCTRL registers.
const NVMCTRL_CTRLB: u64 = 0x4100_4004;
const NVMCTRL_INTFLAG: u64 = 0x4100_4014;
const NVMCTRL_STATUS: u64 = 0x4100_401C;
/// NVMCTRL.INTFLAG bits.
const INTFLAG_READY: u32 = 1 << 0;
const INTFLAG_ERROR: u32 = 1 << 1;
/// NVMCTRL.CTRLB commands, with the execution key.
const NVM_CMD_NOOP: u32 = 0xA501;
const NVM_CMD_WROMCFG: u32 = 0xA520;
const NVM_CMD_CHER: u32 = 0xA521;

/// ROMCFG word holding the DAL 2 setting, and its erased and default values.
const ROMCFG_DAL2: u64 = 0x0D00_0000;
const ROMCFG_DAL2_DEFAULT: u32 = 0xFFFF_FF55;

/// Address of the reset vector of the application in flash.
const RESET_VECTOR: u64 = 0x0C00_0004;

/// Address in SRAM of the idle loop the core runs when there is no application.
const IDLE_LOOP: u32 = 0x2000_0000;
/// Two `b .` instructions.
const IDLE_LOOP_CODE: u32 = 0xE7FE_E7FE;

/// Flash range of the largest device in the family. A reset handler outside of it is not valid.
const FLASH_RANGE: std::ops::Range<u32> = 0x0C00_0000..0x0C02_0000;

/// How long to wait for the Boot ROM to answer a command.
const BOOT_ROM_TIMEOUT: Duration = Duration::from_secs(1);
/// How long to wait for the Boot ROM to answer `CMD_IMODE` after a boot error. It does not
/// answer if it is already in Interactive mode.
const IMODE_AFTER_ERROR_TIMEOUT: Duration = Duration::from_millis(100);
/// How long to wait for the Boot ROM to erase the chip.
const BOOT_ROM_ERASE_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for the NVM controller to finish a command.
const NVM_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to wait for the core to reach the reset handler after leaving Park mode.
const HALT_IN_FLASH_TIMEOUT: Duration = Duration::from_secs(1);

/// Debug sequences for the Microchip PIC32CM PL10 family.
#[derive(Debug, Default)]
pub struct Pic32cmPl10 {
    /// Whether the core must halt at the reset handler after the next reset.
    ///
    /// Vector catch does not work on these devices, because the Boot ROM runs first. The halt
    /// is done with a breakpoint on the reset handler instead.
    halt_after_reset: AtomicBool,
}

fn sequence_error(message: impl Into<String>) -> ArmError {
    ArmDebugSequenceError::SequenceSpecific(message.into().into()).into()
}

impl Pic32cmPl10 {
    /// Create the sequencer for the PIC32CM PL10 family.
    pub fn create() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Reset the device with the cold-plugging sequence: three SWCLK pulses while nRESET is
    /// asserted make the DSU detect the debugger and hold the CPU in CPU Reset Extension.
    fn reset_into_extension(interface: &mut dyn SwdSequence) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        let mut swclk = Pins(0);
        swclk.set_swclk_tck(true);
        let swclk = swclk.0 as u32;

        interface.swj_pins(0, n_reset, 0)?;
        thread::sleep(Duration::from_millis(50));

        // Three SWCLK rising edges while nRESET is low, ending with SWCLK low.
        for _ in 0..3 {
            interface.swj_pins(0, swclk, 0)?;
            interface.swj_pins(swclk, swclk, 0)?;
        }
        interface.swj_pins(0, swclk, 0)?;

        interface.swj_pins(n_reset, n_reset, 0)?;
        thread::sleep(Duration::from_millis(50));

        Ok(())
    }

    /// Reset the device into CPU Reset Extension, and reconnect to it.
    ///
    /// The reset takes the debug port down with it, and invalidates any memory interface that
    /// was opened before, so this returns a new one.
    fn reset_and_reconnect<'i>(
        interface: &'i mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<Box<dyn ArmMemoryInterface + 'i>, ArmError> {
        Self::reset_into_extension(interface)?;
        interface.reinitialize()?;
        interface.memory_interface(ap)
    }

    /// Read the next status the Boot ROM posts on BCC1, or `None` if it posts none in time.
    fn read_boot_rom_status(
        memory: &mut dyn ArmMemoryInterface,
        timeout: Duration,
    ) -> Result<Option<u32>, ArmError> {
        let start = Instant::now();
        loop {
            if memory.read_word_32(DSU_STATUSB)? & STATUSB_BCCD1 != 0 {
                return Ok(Some(memory.read_word_32(DSU_BCC1)?));
            }
            if start.elapsed() >= timeout {
                return Ok(None);
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn is_boot_rom_error(status: u32) -> bool {
        status & STATUS_ERROR_MASK == STATUS_ERROR
    }

    /// Wait until the Boot ROM reports `expected` on BCC1.
    fn wait_boot_rom_status(
        memory: &mut dyn ArmMemoryInterface,
        expected: u32,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            let Some(status) = Self::read_boot_rom_status(memory, remaining)? else {
                return Err(sequence_error(format!(
                    "Timeout waiting for Boot ROM status {expected:#x}"
                )));
            };
            if status == expected {
                return Ok(());
            }
            if Self::is_boot_rom_error(status) {
                return Err(sequence_error(format!(
                    "The Boot ROM reported error {status:#010x}"
                )));
            }
            if status == STATUS_ARG_INVALID {
                return Err(sequence_error(
                    "The Boot ROM rejected the command with STATUS_ARG_INVALID. It may be \
                     disabled by its key value in ROMCFG.",
                ));
            }
            tracing::debug!("PIC32CM PL10: Boot ROM status {status:#x}, waiting for {expected:#x}");
        }
    }

    /// Release the CPU from CPU Reset Extension, so the Boot ROM starts, and return the status
    /// it posts once it has initialized the device: `STATUS_INITCHECK_OK`, or an error.
    fn exit_reset_extension(memory: &mut dyn ArmMemoryInterface) -> Result<u32, ArmError> {
        let statusa = memory.read_word_32(DSU_STATUSA)?;
        if statusa & STATUSA_CRSTEXT0 == 0 {
            tracing::warn!("PIC32CM PL10: the device did not enter CPU Reset Extension");
        }
        memory.write_word_32(DSU_STATUSA, STATUSA_CRSTEXT0)?;
        memory.flush()?;

        // Give the Boot ROM time to start.
        thread::sleep(Duration::from_millis(5));

        let Some(status) = Self::read_boot_rom_status(memory, BOOT_ROM_TIMEOUT)? else {
            return Err(sequence_error(
                "Timeout waiting for the Boot ROM to initialize the device",
            ));
        };
        if status != STATUS_INITCHECK_OK && !Self::is_boot_rom_error(status) {
            tracing::debug!("PIC32CM PL10: unexpected first Boot ROM status {status:#x}");
        }
        Ok(status)
    }

    /// Release the CPU from CPU Reset Extension, and put the Boot ROM into Park mode. Then halt
    /// the core, unless debug access is disabled.
    ///
    /// Returns the debug access level.
    fn reset_to_park(memory: &mut dyn ArmMemoryInterface) -> Result<u32, ArmError> {
        let status = Self::exit_reset_extension(memory)?;
        if Self::is_boot_rom_error(status) {
            // After an error, CMD_EXIT resets the device instead of entering Park mode.
            return Err(sequence_error(format!(
                "The Boot ROM reported error {status:#010x}, Park mode is not available"
            )));
        }

        memory.write_word_32(DSU_BCC0, CMD_EXIT)?;
        memory.flush()?;
        Self::wait_boot_rom_status(memory, STATUS_BOOTOK, BOOT_ROM_TIMEOUT)?;

        let dal = memory.read_word_32(DSU_DAL)? & 0x3;
        tracing::debug!("PIC32CM PL10: DAL = {dal}");
        if dal != 0 {
            let mut dhcsr = Dhcsr(0);
            dhcsr.enable_write();
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
            memory.flush()?;
        }

        Ok(dal)
    }

    /// Release the CPU from CPU Reset Extension, and put the Boot ROM into Interactive mode,
    /// where it accepts commands.
    fn reset_to_interactive_mode(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let status = Self::exit_reset_extension(memory)?;

        memory.write_word_32(DSU_BCC0, CMD_IMODE)?;
        memory.flush()?;

        if !Self::is_boot_rom_error(status) {
            return Self::wait_boot_rom_status(memory, STATUS_CMD_VALID, BOOT_ROM_TIMEOUT);
        }

        // After a boot error, the data sheet says both that the Boot ROM enters Interactive
        // mode by itself, and that it waits for CMD_IMODE. In Interactive mode, it does not
        // answer CMD_IMODE, so no answer means it is already there.
        tracing::warn!("PIC32CM PL10: the Boot ROM reported error {status:#010x}");
        match Self::read_boot_rom_status(memory, IMODE_AFTER_ERROR_TIMEOUT)? {
            Some(STATUS_CMD_VALID) | None => Ok(()),
            Some(status) => Err(sequence_error(format!(
                "The Boot ROM answered CMD_IMODE with {status:#010x}"
            ))),
        }
    }

    /// Answer the Boot ROM's challenge: read the 128-bit challenge, and send a 256-bit response.
    /// The PIC32CM PL10 does not check the response, so it is all zeros.
    fn answer_challenge(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        for _ in 0..4 {
            let start = Instant::now();
            while memory.read_word_32(DSU_STATUSB)? & STATUSB_BCCD1 == 0 {
                if start.elapsed() >= BOOT_ROM_TIMEOUT {
                    return Err(sequence_error("The Boot ROM did not send a full challenge"));
                }
            }
            memory.read_word_32(DSU_BCC1)?;
        }
        for _ in 0..8 {
            let start = Instant::now();
            while memory.read_word_32(DSU_STATUSB)? & STATUSB_BCCD0 != 0 {
                if start.elapsed() >= BOOT_ROM_TIMEOUT {
                    return Err(sequence_error("The Boot ROM did not accept the response"));
                }
            }
            memory.write_word_32(DSU_BCC0, 0)?;
            memory.flush()?;
        }
        Ok(())
    }

    /// Release the Boot ROM Phase Extension, so the Boot ROM leaves Park mode and starts the
    /// application once the core runs.
    fn release_brext(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        memory.write_word_32(DSU_STATUSA, STATUSA_BREXT0)?;
        memory.flush()?;
        Ok(())
    }

    /// Point the halted core at an idle loop in SRAM, with interrupts masked.
    ///
    /// Used instead of Park mode when there is no application to start. Resuming the core after
    /// it was halted in Park mode makes the Boot ROM restart at debug access level 0, and
    /// releasing BREXT0 without an application makes the device reset in a loop, where the core
    /// cannot be accessed either. The idle loop keeps the core accessible, also when it is
    /// resumed.
    fn idle_in_sram(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let reset_vector = memory.read_word_32(RESET_VECTOR)?;
        if reset_vector != 0xFFFF_FFFF {
            tracing::warn!(
                "PIC32CM PL10: the reset vector {reset_vector:#010x} is not a valid reset handler, \
                 the application cannot start"
            );
        }
        tracing::debug!("PIC32CM PL10: no application to start, running an idle loop in SRAM");
        memory.write_word_32(IDLE_LOOP as u64, IDLE_LOOP_CODE)?;
        // CONTROL and PRIMASK: mask interrupts.
        let extra = CORTEX_M_CORE_REGISTERS.other_by_name("EXTRA").unwrap();
        cortex_m::write_core_reg(memory, extra.into(), 1)?;
        cortex_m::write_core_reg(memory, PC.into(), IDLE_LOOP)?;
        // If the device resets later, for example after an application was flashed, the Boot
        // ROM starts the application instead of waiting for the debugger.
        Self::release_brext(memory)
    }

    /// Run the core.
    fn run(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_debugen(true);
        memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        memory.flush()?;
        Ok(())
    }

    /// Read the application's reset handler, if the application can be started.
    ///
    /// Returns `None` if the flash is erased, or if the reset vector is not a Thumb address in
    /// flash. Leaving Park mode then makes the device drop to debug access level 0: with an
    /// erased flash, the Boot ROM resets the device in a loop, and with an invalid reset vector,
    /// the core faults on its first instruction.
    fn application_reset_handler(
        memory: &mut dyn ArmMemoryInterface,
    ) -> Result<Option<u32>, ArmError> {
        let reset_handler = memory.read_word_32(RESET_VECTOR)?;
        if reset_handler == 0xFFFF_FFFF {
            tracing::debug!("PIC32CM PL10: the flash is erased");
            return Ok(None);
        }
        if reset_handler & 1 == 0 || !FLASH_RANGE.contains(&(reset_handler & !1)) {
            tracing::debug!(
                "PIC32CM PL10: the reset vector {reset_handler:#010x} is not a valid reset handler"
            );
            return Ok(None);
        }
        Ok(Some(reset_handler))
    }

    /// Read the debug access level.
    fn debug_access_level(memory: &mut dyn ArmMemoryInterface) -> Result<u32, ArmError> {
        Ok(memory.read_word_32(DSU_DAL)? & 0x3)
    }

    /// Whether the core can be accessed. When the Boot ROM resets the device in a loop, the
    /// debug access level is 0 while the Boot ROM runs, and the configured level is only in
    /// effect for a short time on each boot, so the core stays inaccessible.
    fn is_core_accessible(memory: &mut dyn ArmMemoryInterface) -> Result<bool, ArmError> {
        Ok(Self::debug_access_level(memory)? != 0
            && memory.read_word_32(Dhcsr::get_mmio_address()).is_ok())
    }

    /// Reset into the Boot ROM's Park mode, which restores the configured debug access level.
    ///
    /// The core is left halted, and starts the application when it resumes. If there is no
    /// application that can be started, the core is left halted at an idle loop in SRAM, as
    /// leaving Park mode would drop the debug access level to 0 again.
    ///
    /// `was_inaccessible` tells that the core was not accessible before, to report why.
    ///
    /// Returns the debug access level after the reset, which is 0 if the device is locked.
    fn restore_access(
        interface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
        was_inaccessible: bool,
    ) -> Result<u32, ArmError> {
        let mut memory = Self::reset_and_reconnect(interface, ap)?;
        let dal = Self::reset_to_park(&mut *memory)?;
        if dal == 0 {
            return Ok(0);
        }

        if Self::application_reset_handler(&mut *memory)?.is_none() {
            // Without an application, the Boot ROM resets the device in a loop at debug access
            // level 0, as after a power cycle with an erased flash. This is expected.
            if was_inaccessible {
                tracing::debug!("PIC32CM PL10: restored debug access, there is no application");
            }
            Self::idle_in_sram(&mut *memory)?;
        } else {
            if was_inaccessible {
                tracing::warn!(
                    "PIC32CM PL10: the core was not accessible with an application in flash, the \
                     application may have faulted. Reset into the Boot ROM to restore access"
                );
            }
            // Resuming the core after it was halted in Park mode with BREXT0 set restarts the
            // Boot ROM, which drops the debug access level again. With BREXT0 released, the
            // application starts.
            Self::release_brext(&mut *memory)?;
        }

        Ok(dal)
    }

    /// Leave Park mode with a breakpoint on the application's reset handler, and wait for the
    /// core to halt there. If it does not, halt it wherever it is.
    ///
    /// If there is no application that can be started, stay halted at the idle loop in SRAM
    /// instead. The breakpoint could not stop an invalid reset handler either, as breakpoints
    /// only match in the code region.
    fn halt_in_flash(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let Some(reset_handler) = Self::application_reset_handler(memory)? else {
            return Self::idle_in_sram(memory);
        };
        tracing::debug!("PIC32CM PL10: reset handler at {reset_handler:#010x}");

        let mut bp_ctrl = BpCtrl(0);
        bp_ctrl.set_key(true);
        bp_ctrl.set_enable(true);
        memory.write_word_32(BpCtrl::get_mmio_address(), bp_ctrl.into())?;

        let mut bp_comp = BpCompx(0);
        bp_comp.set_comp((reset_handler >> 2) & 0x7FF_FFFF);
        bp_comp.set_bp_match(if reset_handler & 0x2 == 0 { 0b01 } else { 0b10 });
        bp_comp.set_enable(true);
        memory.write_word_32(BpCompx::get_mmio_address(), bp_comp.into())?;

        Self::release_brext(memory)?;
        Self::run(memory)?;

        let start = Instant::now();
        while start.elapsed() < HALT_IN_FLASH_TIMEOUT {
            // Reads can fail while the Boot ROM runs.
            match memory.read_word_32(Dhcsr::get_mmio_address()) {
                Ok(dhcsr) if Dhcsr(dhcsr).s_halt() => {
                    tracing::debug!(
                        "PIC32CM PL10: halted at the reset handler after {:?}",
                        start.elapsed()
                    );
                    return Ok(());
                }
                Ok(_) => {}
                Err(error) => tracing::debug!("PIC32CM PL10: reading DHCSR failed: {error}"),
            }
            thread::sleep(Duration::from_millis(10));
        }

        tracing::warn!("PIC32CM PL10: the core did not halt at the reset handler, halting it");
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_debugen(true);
        dhcsr.set_c_halt(true);
        memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        memory.flush()?;

        Ok(())
    }

    /// Run the Boot ROM handshake on a device in CPU Reset Extension, then either halt at the
    /// reset handler or let the application run.
    fn boot_from_reset_extension(
        &self,
        memory: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        if Self::reset_to_park(memory)? == 0 {
            return Err(sequence_error(
                "The debug access level is 0, the device is locked. A chip erase unlocks it.",
            ));
        }

        if self.halt_after_reset.load(Ordering::Relaxed) {
            Self::halt_in_flash(memory)
        } else if Self::application_reset_handler(memory)?.is_none() {
            Self::idle_in_sram(memory)?;
            Self::run(memory)
        } else {
            Self::release_brext(memory)?;
            Self::run(memory)
        }
    }

    /// Wait for the NVM controller to be ready, and fail if the last command failed.
    fn nvm_wait_ready(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let start = Instant::now();
        loop {
            let intflag = memory.read_word_32(NVMCTRL_INTFLAG)?;
            if intflag & INTFLAG_READY != 0 {
                if intflag & INTFLAG_ERROR != 0 {
                    let status = memory.read_word_32(NVMCTRL_STATUS)?;
                    memory.write_word_32(NVMCTRL_INTFLAG, INTFLAG_ERROR)?;
                    memory.flush()?;
                    return Err(sequence_error(format!(
                        "The NVM controller reported an error, STATUS = {status:#x}"
                    )));
                }
                return Ok(());
            }
            if start.elapsed() >= NVM_TIMEOUT {
                return Err(sequence_error("Timeout waiting for the NVM controller"));
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Run an NVM controller command.
    fn nvm_command(memory: &mut dyn ArmMemoryInterface, command: u32) -> Result<(), ArmError> {
        // Clear errors left by earlier commands.
        memory.write_word_32(NVMCTRL_INTFLAG, INTFLAG_ERROR)?;
        Self::nvm_wait_ready(memory)?;
        memory.write_word_32(NVMCTRL_CTRLB, command)?;
        memory.flush()?;
        Ok(())
    }

    /// Erase the flash with the NVM controller. This requires debug access level 2.
    fn erase_with_nvmctrl(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        Self::nvm_command(memory, NVM_CMD_CHER)?;
        Self::nvm_wait_ready(memory)?;
        Self::nvm_command(memory, NVM_CMD_NOOP)?;
        Self::nvm_wait_ready(memory)?;

        // CHER also erases ROMCFG, including the DAL2 fuse, which would leave the device at debug
        // access level 0 after the next reset. Restore it.
        if memory.read_word_32(ROMCFG_DAL2)? == 0xFFFF_FFFF {
            tracing::debug!("PIC32CM PL10: restoring the DAL 2 setting in ROMCFG");
            Self::nvm_command(memory, NVM_CMD_WROMCFG)?;
            memory.write_word_32(ROMCFG_DAL2, ROMCFG_DAL2_DEFAULT)?;
            memory.flush()?;
            Self::nvm_command(memory, NVM_CMD_NOOP)?;
            Self::nvm_wait_ready(memory)?;
        }

        Ok(())
    }

    /// Erase the chip with the Boot ROM's CMD_CE_ALL command. This works at any debug access level,
    /// unless the chip erase key was changed.
    fn erase_with_boot_rom(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        Self::reset_to_interactive_mode(memory)?;

        memory.write_word_32(DSU_BCC0, CMD_CE_ALL)?;
        memory.flush()?;
        Self::wait_boot_rom_status(memory, STATUS_CHALLENGE, BOOT_ROM_TIMEOUT)?;
        Self::answer_challenge(memory)?;
        Self::wait_boot_rom_status(memory, STATUS_CMD_VALID, BOOT_ROM_TIMEOUT)?;
        Self::wait_boot_rom_status(memory, STATUS_OK, BOOT_ROM_ERASE_TIMEOUT)
    }

    /// Erase the whole flash, and reset the debug access level to 2.
    ///
    /// Leaves the core halted in the Boot ROM's Park mode.
    fn chip_erase(
        interface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("PIC32CM PL10: erasing the chip");

        let dal = {
            let mut memory = Self::reset_and_reconnect(interface, ap)?;
            let dal = Self::reset_to_park(&mut *memory)?;
            if dal == 2 {
                Self::erase_with_nvmctrl(&mut *memory)?;
            }
            dal
        };
        if dal != 2 {
            let mut memory = Self::reset_and_reconnect(interface, ap)?;
            Self::erase_with_boot_rom(&mut *memory)?;
        }

        let mut memory = Self::reset_and_reconnect(interface, ap)?;
        let dal = Self::reset_to_park(&mut *memory)?;
        if dal != 2 {
            return Err(sequence_error(format!(
                "The debug access level is {dal} after the chip erase"
            )));
        }

        tracing::info!("PIC32CM PL10: chip erase done");
        Ok(())
    }
}

impl ArmDebugSequence for Pic32cmPl10 {
    /// Restore debug access when the core is not accessible.
    ///
    /// The device drops to DAL 0 when the Boot ROM cannot start the application, for example
    /// after a power cycle with an erased flash, or after the application faults. Then only the
    /// DSU is accessible. The device then resets in a loop, and the configured access level is
    /// only in effect for a short time on each boot. Resetting into the Boot ROM's Park mode
    /// restores the configured access level. If DAL 0 is configured, the device is locked, and
    /// only a chip erase unlocks it.
    ///
    /// Without a valid application, the device is also reset into Park mode when the core is
    /// accessible, so the core can be resumed without leaving Park mode.
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &Permissions,
    ) -> Result<(), ArmError> {
        let mut inaccessible = false;
        {
            let mut memory = interface.memory_interface(default_ap)?;
            if Self::is_core_accessible(&mut *memory)? {
                if Self::application_reset_handler(&mut *memory)?.is_some() {
                    return Ok(());
                }
                tracing::debug!("PIC32CM PL10: no application, resetting into the Boot ROM");
            } else if memory.read_word_32(DSU_STATUSA)? & STATUSA_CRSTEXT0 != 0 {
                // Connecting under reset: `reset_hardware_deassert` runs the Boot ROM handshake.
                return Ok(());
            } else {
                tracing::debug!(
                    "PIC32CM PL10: the core is not accessible, resetting into the Boot ROM"
                );
                inaccessible = true;
            }
        }

        if Self::restore_access(interface, default_ap, inaccessible)? != 0 {
            return Ok(());
        }

        if let Err(MissingPermissions(permission)) = permissions.erase_all() {
            return Err(ArmError::MissingPermissions(format!(
                "The device is locked, with debug access level 0. A chip erase is required to \
                 unlock it. Re-run with the '{permission}' permission granted."
            )));
        }
        tracing::warn!("PIC32CM PL10: the device is locked, erasing it");
        Self::chip_erase(interface, default_ap)
    }

    /// Ignore a failed core debug deconfiguration when the debug access level is 0.
    ///
    /// When the core resumes after it was halted in Park mode, the Boot ROM restarts, and the
    /// device can drop to debug access level 0 at any time during shutdown. The core debug
    /// registers then cannot be accessed, and the Boot ROM has already reset the debug state.
    fn debug_core_stop(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        let result = self
            .debug_core_stop_default(interface, core_type)
            .and_then(|()| interface.flush());
        let Err(error) = result else {
            return Ok(());
        };

        if interface.read_word_32(DSU_DAL)? & 0x3 == 0 {
            tracing::debug!(
                "PIC32CM PL10: debug access level is 0, the core debug stop failed: {error}"
            );
            return Ok(());
        }
        Err(error)
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        Some(Self::create())
    }

    fn reset_hardware_assert(&self, interface: &mut dyn DebugPortWire) -> Result<(), ArmError> {
        Self::reset_into_extension(&mut SwdSequenceShim::from(interface))
    }

    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        // nRESET was already released by `reset_hardware_assert`, the CPU is held in
        // CPU Reset Extension.
        let mut memory = probe.memory_interface(default_ap)?;
        self.boot_from_reset_extension(&mut *memory)
    }

    fn reset_catch_set(
        &self,
        _core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        self.halt_after_reset.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        self.halt_after_reset.store(false, Ordering::Relaxed);

        let mut bp_ctrl = BpCtrl(0);
        bp_ctrl.set_key(true);
        core.write_word_32(BpCtrl::get_mmio_address(), bp_ctrl.into())?;
        core.write_word_32(BpCompx::get_mmio_address(), 0)?;
        core.flush()?;

        Ok(())
    }

    fn reset_system(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let ap = memory.fully_qualified_address();
        let interface = memory.get_arm_debug_interface()?;
        let mut memory = Self::reset_and_reconnect(interface, &ap)?;
        self.boot_from_reset_extension(&mut *memory)
    }
}

impl DebugEraseSequence for Pic32cmPl10 {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        Self::chip_erase(interface, &ap)?;

        // The chip was reset, so the session must reconnect.
        Err(ArmError::ReAttachRequired)
    }
}
