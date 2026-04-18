//! Sequences for CC23xx/CC27xx devices.
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::MemoryMappedRegister;
use crate::Session;
use crate::architecture::arm::ArmDebugInterface;
use crate::architecture::arm::DapAccess;
use crate::architecture::arm::DapProbe;
use crate::architecture::arm::armv6m::{Aircr, BpCtrl, Demcr, Dhcsr};
use crate::architecture::arm::core::cortex_m;
use crate::architecture::arm::dp::{Abort, Ctrl, DebugPortError, DpAccess, DpAddress, SelectV1};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, cortex_m_core_start};
use crate::architecture::arm::{ArmError, FullyQualifiedApAddress};
use crate::flashing::DebugFlashSequence;
use probe_rs_target::CoreType;
use super::saci;
use super::saci::{
    SaciResult,
    BOOT_STATUS_APP_WAITLOOP_DBGPROBE,
    BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE,
    BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE,
};

/// Marker struct for debug and flash sequencing on CC23xx/CC27xx family parts.
#[derive(Debug)]
pub struct CC23xxCC27xx {
    /// Chip name - used when additional targets are added.
    _name: String,
    /// Tracks whether the ROM is currently in the boot wait loop.
    boot_loop: AtomicBool,
    /// Shared flag set during host-side flash programming.
    ///
    /// When true, debug_port_start skips EXIT_SACI so the ROM SACI handler
    /// stays active for flash operations. Shared with CC23xxCC27xxFlashSequence
    /// via Arc and reset to false when the flash sequence is dropped.
    saci_flash_mode: Arc<AtomicBool>,
}

/// Flash memory region types for CC23xx/CC27xx devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashRegion {
    /// Main application flash.
    Main,
    /// Customer Configuration (CCFG) sector.
    Ccfg,
    /// Security Configuration (SCFG) sector - CC27xx only.
    Scfg,
}

impl CC23xxCC27xx {
    /// Create the sequencer for the CC23xx/CC27xx family.
    pub fn create(name: String) -> Arc<Self> {
        Arc::new(Self {
            _name: name,
            boot_loop: AtomicBool::new(false),
            saci_flash_mode: Arc::new(AtomicBool::new(false)),
        })
    }

    fn is_in_boot_loop(&self) -> bool {
        self.boot_loop.load(Ordering::SeqCst)
    }
}

impl ArmDebugSequence for CC23xxCC27xx {
    fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Check whether the caller requested a halt on reset.
        let demcr = Demcr(probe.read_word_32(Demcr::get_mmio_address())?);
        let mut bpt_ctrl = BpCtrl(probe.read_word_32(BpCtrl::get_mmio_address())?);

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        probe.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        probe.flush().ok();
        thread::sleep(Duration::from_millis(10));

        let ap = probe.fully_qualified_address();
        let interface = probe.get_arm_debug_interface()?;

        interface.reinitialize()?;
        self.debug_core_start(interface, &ap, core_type, debug_base, None)?;

        if demcr.vc_corereset() {
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();

            probe.write_word_32(Dhcsr::get_mmio_address(), value.into())?;
        }

        bpt_ctrl.set_key(true);
        probe.write_word_32(BpCtrl::get_mmio_address(), bpt_ctrl.into())?;

        Ok(())
    }

    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // TODO: copy-pasted from the default Trait implementation; the
        // CC23xx/CC27xx-specific logic is appended at the end.
        // Source: debug_port_start in probe-rs/src/architecture/arm/sequences.rs

        let mut abort = Abort(0);
        abort.set_dapabort(true);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);
        interface.write_dp_register(dp, abort)?;

        interface.write_dp_register(dp, SelectV1(0))?;

        let ctrl = interface.read_dp_register::<Ctrl>(dp)?;

        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

        if powered_down {
            tracing::info!("Debug port {dp:x?} is powered down, powering up");
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            interface.write_dp_register(dp, ctrl)?;

            let start = Instant::now();
            loop {
                let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
            }

            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            ctrl.set_mask_lane(0b1111);
            interface.write_dp_register(dp, ctrl)?;

            let ctrl_reg: Ctrl = interface.read_dp_register(dp)?;
            if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
                tracing::error!("Debug power request failed");
                return Err(DebugPortError::TargetPowerUpFailed.into());
            }
        }
        // End of copy-paste from default debug_port_start.

        // CC23xx/CC27xx-specific: check device state via CFG-AP and exit SACI
        // if the device is in SACI mode and we are not in a flash operation.
        let mut device_status = saci::read_device_status(interface)?;

        if !device_status.ahb_ap_available() {
            if self.saci_flash_mode.load(Ordering::SeqCst) {
                // Flash programming is in progress; stay in SACI mode so the
                // ROM flash handler remains active for subsequent SACI commands.
                tracing::info!(
                    "CC23xx/CC27xx: debug_port_start in flash mode, leaving SACI active"
                );
                return Ok(());
            }

            // Normal debug session: exit SACI so the AHB-AP becomes accessible.
            // debug_port_connect already asserted nRESET so the ROM started fresh
            // with isExitAllowed=true, meaning DEBUG_EXIT_SACI_HALT will succeed.
            saci::send_command(interface, saci::cmd::DEBUG_EXIT_SACI_HALT)?;

            thread::sleep(Duration::from_millis(30));
            device_status = saci::read_device_status(interface)?;

            match device_status.boot_status() {
                BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE
                | BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE
                | BOOT_STATUS_APP_WAITLOOP_DBGPROBE => {
                    tracing::info!("BOOT_WAITLOOP_DBGPROBE");
                    self.boot_loop.store(true, Ordering::SeqCst);
                }
                _ => {
                    if !device_status.ahb_ap_available() {
                        tracing::warn!(
                            "CC23xx/CC27xx: Device is still in SACI mode after EXIT_SACI_HALT"
                        );
                    }
                }
            }
        }

        Ok(())
    }

    fn debug_core_start(
        &self,
        interface: &mut dyn ArmDebugInterface,
        core_ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _debug_base: Option<u64>,
        _cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if self.is_in_boot_loop() {
            // Step 1: halt the CPU.
            let mut dhcsr = Dhcsr(0);
            dhcsr.set_c_halt(true);
            dhcsr.set_c_debugen(true);
            dhcsr.enable_write();

            let mut memory = interface.memory_interface(core_ap)?;
            memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

            // Wait for the CPU to halt.
            dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
            while !dhcsr.s_halt() {
                dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);
            }

            // Step 2: write R3 = 0 to exit the ROM boot wait loop.
            cortex_m::write_core_reg(memory.deref_mut(), crate::RegisterId(3), 0x00000000)?;

            // Step 3: clear the boot loop flag.
            self.boot_loop.store(false, Ordering::SeqCst);
        }

        // Step 4: start the core normally.
        let mut core = interface.memory_interface(core_ap)?;
        cortex_m_core_start(&mut *core)
    }

    fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        Some(Arc::new(CC23xxCC27xxFlashSequence::new_with_flag(
            Arc::clone(&self.saci_flash_mode),
        )))
    }

    /// Assert and deassert nRESET on every connect.
    ///
    /// Matches OpenOCD's behaviour for CC27xx: every init begins with
    /// `adapter assert srst` so the device always starts from a known state
    /// (ROM SACI handler active, isExitAllowed=true). Without this, a device
    /// left stuck in SACI mode with isExitAllowed=false cannot exit SACI via
    /// software commands alone.
    ///
    /// After the reset the default SWD reconnect sequence runs, and
    /// debug_port_start then exits SACI (or stays in it during flash mode).
    fn debug_port_connect(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("CC23xx/CC27xx: Asserting nRESET before SWD connect (OpenOCD-compatible)");

        // Assert nRESET (drive low).
        self.reset_hardware_assert(interface)?;
        thread::sleep(Duration::from_millis(5));

        // Deassert nRESET: set both pin_output and pin_select bits so the
        // probe drives nRESET high.
        let mut n_reset = crate::architecture::arm::traits::Pins(0);
        n_reset.set_nreset(true);
        let _ = interface.swj_pins(n_reset.0 as u32, n_reset.0 as u32, 0)?;

        // Give the ROM time to reach the SACI handler before the SWD connect
        // sequence reads DPIDR (60 ms matches OpenOCD's timing).
        thread::sleep(Duration::from_millis(60));

        let default = crate::architecture::arm::sequences::DefaultArmSequence::create();
        default.debug_port_connect(interface, dp)
    }
}

// ---- Flash sequence ---------------------------------------------------------

/// Address boundary for the CCFG region on CC23xx/CC27xx devices.
const CCFG_START: u64 = 0x4E02_0000;
/// Address boundary for the SCFG region on CC23xx/CC27xx devices.
const SCFG_START: u64 = 0x4E04_0000;

/// Host-side flash programming implementation for CC23xx/CC27xx devices.
///
/// Implements DebugFlashSequence by issuing SACI commands through the SEC-AP
/// rather than loading a flash algorithm into target RAM.
///
/// The saci_flash_mode flag is shared with CC23xxCC27xx. While this struct is
/// alive the flag is true, which tells debug_port_start to skip the EXIT_SACI
/// command so the ROM SACI handler stays active. The flag is automatically reset
/// to false when this struct is dropped.
#[derive(Debug)]
pub struct CC23xxCC27xxFlashSequence {
    /// Shared flag with CC23xxCC27xx to suppress EXIT_SACI during flash.
    saci_flash_mode: Arc<AtomicBool>,
}

impl CC23xxCC27xxFlashSequence {
    /// Create a flash sequence sharing the saci_flash_mode flag with CC23xxCC27xx.
    ///
    /// The flag is set to true immediately. CC23xxCC27xx::debug_flash_sequence()
    /// passes its own Arc<AtomicBool> here so that both structs observe the same
    /// flag. While true, debug_port_start skips EXIT_SACI_HALT so the ROM SACI
    /// handler stays active across reinitialize() calls during flash programming.
    /// Drop resets it to false so normal debug sessions are unaffected afterward.
    pub fn new_with_flag(saci_flash_mode: Arc<AtomicBool>) -> Self {
        saci_flash_mode.store(true, Ordering::SeqCst);
        Self { saci_flash_mode }
    }

    /// Fallback constructor with no shared flag, for tests or standalone use.
    pub fn new() -> Self {
        Self::new_with_flag(Arc::new(AtomicBool::new(false)))
    }

    /// Reset the device via hardware nRESET and wait for SACI mode.
    ///
    /// debug_port_start exits SACI mode so the AHB-AP is accessible for normal
    /// debug. Before flash commands can be sent the device must be reset back
    /// into SACI mode via a hardware nRESET.
    ///
    /// SYSRESETRQ (writing AIRCR) does not trigger the ROM to re-enter SACI;
    /// only a hardware reset does. This matches the OpenOCD sequence in
    /// ti_cc27xx.cfg: assert srst -> 5 ms -> deassert -> 60 ms -> dap init -> 100 ms.
    fn reset_into_saci_mode(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        tracing::info!("CC23xx/CC27xx: Re-initializing to enter SACI mode for flash programming");

        // reinitialize calls debug_port_connect which asserts nRESET, causing
        // the ROM to re-enter SACI mode. debug_port_start will see
        // saci_flash_mode == true and skip EXIT_SACI.
        interface.reinitialize()?;

        // Give the ROM time to reach the SACI handler after reinitialize.
        thread::sleep(Duration::from_millis(100));

        // Confirm SACI mode
        let start = Instant::now();
        loop {
            if matches!(saci::read_device_status(interface), Ok(s) if !s.ahb_ap_available()) {
                tracing::info!("CC23xx/CC27xx: Device is in SACI mode, ready for flash");
                return Ok(());
            }
            if start.elapsed() >= Duration::from_secs(3) {
                tracing::error!(
                    "CC23xx/CC27xx: Timeout waiting for SACI mode. \
                     Ensure nRESET is connected to the debug probe."
                );
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn program_main(
        &self,
        interface: &mut dyn DapAccess,
        address: u64,
        data: &[u8],
    ) -> Result<(), ArmError> {
        let byte_count = data.len() as u32;
        let header = saci::make_header(saci::cmd::FLASH_PROG_MAIN_SECTOR, byte_count);
        let addr_word = address as u32;

        let mut words = vec![header, saci::cmd::FLASH_KEY, addr_word];
        words.extend(saci::pack_words(data, 0xFF));

        saci::send_words(interface, &words, Duration::from_millis(200))?;
        let response = saci::read_response(interface, Duration::from_secs(5))?;
        saci::check_result(
            response,
            &format!("FLASH_PROG_MAIN_SECTOR at 0x{address:08X}"),
        )?;
        Ok(())
    }

    /// Program the CCFG sector using FLASH_PROG_CCFG_SECTOR
    ///
    /// Pads data to exactly 512 words (2048 bytes) with 0xFF. Sets skip_user_rec=1.
    fn program_ccfg(&self, interface: &mut dyn DapAccess, data: &[u8]) -> Result<(), ArmError> {
        // skip_user_rec = bit 0 of cmd_specific
        let header = saci::make_header(saci::cmd::FLASH_PROG_CCFG_SECTOR, 0x0001);

        // Pad to exactly 2048 bytes.
        let mut padded = vec![0xFFu8; 2048];
        let copy_len = data.len().min(2048);
        padded[..copy_len].copy_from_slice(&data[..copy_len]);

        let mut words = vec![header, saci::cmd::FLASH_KEY];
        words.extend(saci::pack_words(&padded, 0xFF));

        saci::send_words(interface, &words, Duration::from_millis(200))?;
        let response = saci::read_response(interface, Duration::from_secs(5))?;
        saci::check_result(response, "FLASH_PROG_CCFG_SECTOR")?;
        Ok(())
    }

    /// Program the SCFG sector using FLASH_PROG_SCFG_SECTOR (0x1A).
    fn program_scfg(&self, interface: &mut dyn DapAccess, data: &[u8]) -> Result<(), ArmError> {
        let byte_count = data.len() as u32;
        let header = saci::make_header(saci::cmd::FLASH_PROG_SCFG_SECTOR, byte_count);

        let mut words = vec![header, saci::cmd::FLASH_KEY];
        words.extend(saci::pack_words(data, 0xFF));

        saci::send_words(interface, &words, Duration::from_millis(200))?;
        let response = saci::read_response(interface, Duration::from_secs(5))?;
        saci::check_result(response, "FLASH_PROG_SCFG_SECTOR")?;
        Ok(())
    }
}

impl Drop for CC23xxCC27xxFlashSequence {
    fn drop(&mut self) {
        // Clear the flash mode flag on drop so that any subsequent debug session
        // (including error and panic paths) correctly exits SACI on the next
        // debug_port_start call. Without this, a failed flash operation would
        // leave the flag set and make the AHB-AP permanently inaccessible until
        // the next probe-rs process restart.
        self.saci_flash_mode.store(false, Ordering::SeqCst);
    }
}

impl Default for CC23xxCC27xxFlashSequence {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugFlashSequence for CC23xxCC27xxFlashSequence {
    fn prepare_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        let interface = session.get_arm_interface()?;

        // Ensure the device is in SACI mode before any flash operation. Handles
        // both the initial call (device was in normal debug mode after
        // debug_port_start exited SACI) and repeated calls after finish_flash
        // re-entered normal debug mode (e.g. standalone verify pass).
        if matches!(saci::read_device_status(interface), Ok(s) if s.ahb_ap_available()) {
            tracing::info!("CC23xx/CC27xx: prepare_flash -- device not in SACI, re-entering");
            self.reset_into_saci_mode(interface)?;
        }
        Ok(())
    }

    fn erase_all(&self, session: &mut Session) -> Result<(), crate::Error> {
        let interface = session.get_arm_interface()?;

        tracing::info!("CC23xx/CC27xx: Chip erase via SACI FLASH_ERASE_CHIP (0x09)");

        // FLASH_ERASE_CHIP (0x09): [header, FLASH_KEY]
        let header = saci::make_header(saci::cmd::FLASH_ERASE_CHIP, 0);
        let words = [header, saci::cmd::FLASH_KEY];
        saci::send_words(interface, &words, Duration::from_millis(200))?;

        let response = saci::read_response(interface, Duration::from_secs(30))?;
        saci::check_result(response, "FLASH_ERASE_CHIP")?;

        tracing::info!("CC23xx/CC27xx: Chip erase completed");
        Ok(())
    }

    fn program(
        &self,
        session: &mut Session,
        address: u64,
        data: &[u8],
    ) -> Result<(), crate::Error> {
        tracing::debug!(
            "CC23xx/CC27xx: Programming {} bytes at 0x{:08X}",
            data.len(),
            address
        );

        let interface = session.get_arm_interface()?;

        if address >= SCFG_START {
            self.program_scfg(interface, data)?;
        } else if address >= CCFG_START {
            self.program_ccfg(interface, data)?;
        } else {
            self.program_main(interface, address, data)?;
        }
        Ok(())
    }

    fn verify(
        &self,
        session: &mut Session,
        address: u64,
        data: &[u8],
    ) -> Result<bool, crate::Error> {
        tracing::debug!(
            "CC23xx/CC27xx: Verifying {} bytes at 0x{:08X}",
            data.len(),
            address
        );

        let interface = session.get_arm_interface()?;

        if address >= SCFG_START {
            // FLASH_VERIFY_SCFG_SECTOR (0x1B): [header(check_exp_crc=1), expCrc32]
            //
            // The ROM verifies CRC over only the first 0xE4 (228) bytes of SCFG,
            // matching the range covered by Scfg::update_crcs and OpenOCD's
            // SCFG_CONTENT_SIZE constant. The trailing key-ring slots are excluded.
            const SCFG_CRC_BYTE_COUNT: usize = 0xE4;
            let crc_data = &data[..data.len().min(SCFG_CRC_BYTE_COUNT)];
            let expected_crc = saci::crc32_iso_hdlc(crc_data);
            let header = saci::make_header(saci::cmd::FLASH_VERIFY_SCFG_SECTOR, 0x0001);
            let words = [header, expected_crc];
            saci::send_words(interface, &words, Duration::from_millis(100))?;
            let response = saci::read_response(interface, Duration::from_secs(10))?;
            let result = SaciResult::from(((response >> 16) & 0xFF) as u8);
            match result {
                SaciResult::Success => Ok(true),
                SaciResult::Crc32Mismatch => Ok(false),
                _ => Err(ArmError::Other(format!(
                    "SACI FLASH_VERIFY_SCFG_SECTOR failed: {result:?}"
                ))
                .into()),
            }
        } else if address >= CCFG_START {
            // FLASH_VERIFY_CCFG_SECTOR (0x11): check_exp_crcs=0, skip_user_rec=1.
            // Lets the ROM verify its own embedded CRCs rather than requiring the
            // host to supply external CRC values.
            let header = saci::make_header(saci::cmd::FLASH_VERIFY_CCFG_SECTOR, 0x0002);
            let words = [header, 0u32, 0u32, 0u32, 0u32];
            saci::send_words(interface, &words, Duration::from_millis(100))?;
            let response = saci::read_response(interface, Duration::from_secs(10))?;
            let result = SaciResult::from(((response >> 16) & 0xFF) as u8);
            match result {
                SaciResult::Success => Ok(true),
                SaciResult::Crc32Mismatch | SaciResult::BlankCheckFailed => Ok(false),
                _ => Err(ArmError::Other(format!(
                    "SACI FLASH_VERIFY_CCFG_SECTOR failed: {result:?}"
                ))
                .into()),
            }
        } else {
            // FLASH_VERIFY_MAIN_SECTORS (0x10): [header, firstSectorAddr, byteCount, expCrc32]
            let expected_crc = saci::crc32_iso_hdlc(data);
            let header = saci::make_header(saci::cmd::FLASH_VERIFY_MAIN_SECTORS, 0);
            let words = [header, address as u32, data.len() as u32, expected_crc];
            saci::send_words(interface, &words, Duration::from_millis(100))?;
            let response = saci::read_response(interface, Duration::from_secs(10))?;
            let result = SaciResult::from(((response >> 16) & 0xFF) as u8);
            match result {
                SaciResult::Success => Ok(true),
                SaciResult::Crc32Mismatch => Ok(false),
                _ => Err(ArmError::Other(format!(
                    "SACI FLASH_VERIFY_MAIN_SECTORS failed: {result:?}"
                ))
                .into()),
            }
        }
    }

    fn supports_sector_erase(&self) -> bool {
        false
    }

    fn finish_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        // The session is reused between the flash operation and post-flash debug
        // access, so the device must be out of SACI mode before we return.
        //
        // Sequence (matches OpenOCD cc27xx reset_halt after programming):
        // 1. Clear saci_flash_mode so the next debug_port_start exits SACI.
        // 2. Call reinitialize which triggers debug_port_connect (nRESET -> SACI)
        //    then debug_port_start (EXIT_SACI_HALT -> device boots, AHB-AP available).
        tracing::info!("CC23xx/CC27xx: Flash complete, exiting SACI for normal debug access");

        // Clear the flag BEFORE reinitialize so debug_port_start sends
        // EXIT_SACI_HALT instead of staying in SACI mode.
        self.saci_flash_mode.store(false, Ordering::SeqCst);

        let interface = session.get_arm_interface()?;
        interface.reinitialize()?;

        Ok(())
    }
}
