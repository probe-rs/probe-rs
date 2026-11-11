//! Sequences for CC23xx/CC27xx devices.
use std::ops::DerefMut;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::saci;
use super::saci::{
    BOOT_STATUS_APP_WAITLOOP_DBGPROBE, BOOT_STATUS_BLDR_WAITLOOP_DBGPROBE,
    BOOT_STATUS_BOOT_WAITLOOP_DBGPROBE, SaciResult,
};
use crate::MemoryInterface;
use crate::MemoryMappedRegister;
use crate::Session;
use crate::architecture::arm::ArmDebugInterface;
use crate::architecture::arm::DapAccess;
use crate::architecture::arm::armv6m::{BpCtrl, Demcr, Dhcsr};
use crate::architecture::arm::core::cortex_m;
use crate::architecture::arm::dp::DpAddress;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{
    ArmDebugSequence, DebugEraseSequence, cortex_m_core_start,
};
use crate::architecture::arm::traits::DebugPortWire;
use crate::architecture::arm::{ArmError, FullyQualifiedApAddress};
use crate::config::NvmRegion;
use crate::flashing::{
    CompareContext, CompareCost, CompareOutcome, DebugFlashSequence, FlashCompare, FlashError,
    FlashLoader, SectorCheck,
};
use crate::probe::DebugProbeError;
use probe_rs_target::CoreType;

/// Marker struct for debug and flash sequencing on CC23xx/CC27xx family parts.
#[derive(Debug)]
pub struct CC23xxCC27xx {
    /// Whether the chip is a CC27xx, which has a RAM flash algorithm.
    cc27xx: bool,
    /// Tracks whether the ROM is currently in the boot wait loop.
    boot_loop: AtomicBool,
    /// Shared flag set during host-side flash programming.
    ///
    /// When true, debug_port_start skips EXIT_SACI so the ROM SACI handler
    /// stays active for flash operations. Shared with CC23xxCC27xxFlashSequence
    /// via Arc and reset to false when the flash sequence is dropped.
    saci_flash_mode: Arc<AtomicBool>,
    /// AHB-AP state as of the last debug_port_start.
    ahb_ap_available: AtomicBool,
    /// Whether the current download goes through SACI rather than the RAM flash algorithm.
    ///
    /// Set by the flash sequence for each download.
    saci_download: Arc<AtomicBool>,
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
            cc27xx: name.starts_with("CC27"),
            boot_loop: AtomicBool::new(false),
            saci_flash_mode: Arc::new(AtomicBool::new(false)),
            ahb_ap_available: AtomicBool::new(false),
            saci_download: Arc::new(AtomicBool::new(true)),
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

        let ap = probe.fully_qualified_address();
        let interface = probe.get_arm_debug_interface()?;

        // Reset from the SEC-AP as CCS does. The ROM then waits in SACI.
        // Unlike an AIRCR write, nothing else is sent while the device resets.
        saci::reset_device(interface);
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
        self.debug_port_start_default(interface, dp)?;

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
                self.ahb_ap_available.store(false, Ordering::SeqCst);
                return Ok(());
            }

            // Normal debug session: exit SACI so the AHB-AP becomes accessible.
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
                            "CC23xx/CC27xx: Device stayed in SACI mode (no valid image or debug locked), only flashing is available"
                        );
                    }
                }
            }
        }

        self.ahb_ap_available
            .store(device_status.ahb_ap_available(), Ordering::SeqCst);

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
        // A blank device cannot leave SACI, so there is no AHB-AP yet.
        // Skip core setup so it can still be flashed over SACI.
        if !self.ahb_ap_available.load(Ordering::SeqCst) {
            return Ok(());
        }

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

    fn on_attach(
        &self,
        _interface: &mut dyn ArmDebugInterface,
        _ap: &FullyQualifiedApAddress,
        _core_type: CoreType,
        _target: &mut crate::Target,
    ) -> Result<(), ArmError> {
        // Report the core as disabled until the device has left SACI.
        if !self.ahb_ap_available.load(Ordering::SeqCst) {
            return Err(ArmError::CoreDisabled);
        }
        Ok(())
    }

    fn debug_flash_sequence(&self) -> Option<Arc<dyn DebugFlashSequence>> {
        Some(Arc::new(CC23xxCC27xxFlashSequence {
            saci_flash_mode: Arc::clone(&self.saci_flash_mode),
            saci_download: Arc::clone(&self.saci_download),
            cc27xx: self.cc27xx,
            xcfg_pending: AtomicU8::new(0),
        }))
    }

    fn flash_compare(&self) -> Option<Arc<dyn FlashCompare>> {
        if !self.saci_download.load(Ordering::SeqCst) {
            return None;
        }
        Some(Arc::new(SaciCompare))
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        if !self.cc27xx {
            return None;
        }
        Some(Arc::new(CC27xxEraseSequence {
            saci_flash_mode: Arc::clone(&self.saci_flash_mode),
        }))
    }

    /// Connect without a reset, so a running application keeps running.
    ///
    /// When the debug port does not answer, pulse nRESET and try again, as the
    /// TI OpenOCD scripts do on every connect.
    fn debug_port_setup(
        &self,
        interface: &mut dyn DebugPortWire,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        let Err(error) = self.debug_port_setup_default(interface, dp) else {
            return Ok(());
        };

        // Assert nRESET (drive low). Without a reset line, there is nothing to retry.
        match self.reset_hardware_assert(interface) {
            Err(ArmError::Probe(DebugProbeError::ResetLineNotConnected)) => return Err(error),
            result => result?,
        }
        tracing::info!("CC23xx/CC27xx: no answer ({error}), retrying after a pin reset");
        thread::sleep(Duration::from_millis(5));

        // Deassert nRESET: set both pin_output and pin_select bits so the
        // probe drives nRESET high.
        let mut n_reset = crate::architecture::arm::traits::Pins(0);
        n_reset.set_nreset(true);
        let _ = interface.swj_pins(n_reset, n_reset, Duration::ZERO)?;

        // Give the ROM time to reach the SACI handler before the SWD connect
        thread::sleep(Duration::from_millis(60));

        self.debug_port_setup_default(interface, dp)
    }
}

// ---- Flash sequence ---------------------------------------------------------

/// Address boundary for the CCFG region on CC23xx/CC27xx devices.
const CCFG_START: u64 = 0x4E02_0000;
/// Address boundary for the SCFG region on CC23xx/CC27xx devices.
const SCFG_START: u64 = 0x4E04_0000;
/// Size of the CCFG.
const CCFG_SIZE: u64 = 0x800;
/// Size of the SCFG.
const SCFG_SIZE: usize = 0x400;

/// FLASH_ERASE_CHIP, FLASH_ERASE_MAIN_APP: keep the sectors that the CCFG retains.
const RETAIN_SECTORS: u32 = 1 << 0;
/// FLASH_VERIFY_CCFG_SECTOR: skip the CRC check of the user record.
const SKIP_USER_RECORD: u32 = 1 << 1;
/// FLASH_VERIFY_CCFG_SECTOR: check that the sector is blank.
const BLANK_CHECK: u32 = 1 << 15;

/// Host-side flash programming implementation for CC23xx/CC27xx devices.
///
/// Implements DebugFlashSequence by issuing SACI commands through the SEC-AP
/// rather than loading a flash algorithm into target RAM.
///
/// The saci_flash_mode flag is shared with CC23xxCC27xx. While it is set, debug_port_start
/// skips the EXIT_SACI command so the ROM SACI handler stays active. prepare_flash sets it,
/// and finish_flash or dropping this struct clears it.
#[derive(Debug)]
pub struct CC23xxCC27xxFlashSequence {
    /// Shared flag with CC23xxCC27xx to suppress EXIT_SACI during flash.
    saci_flash_mode: Arc<AtomicBool>,
    /// Shared with CC23xxCC27xx: whether the current download goes through SACI.
    saci_download: Arc<AtomicBool>,
    /// Whether the chip is a CC27xx, which has a RAM flash algorithm.
    cc27xx: bool,
    /// The configuration sectors that the current download still has to program through SACI:
    /// [`PENDING_CCFG`] and [`PENDING_SCFG`].
    xcfg_pending: AtomicU8,
}

const PENDING_CCFG: u8 = 1 << 0;
const PENDING_SCFG: u8 = 1 << 1;

/// Reset the device and wait for SACI mode, with `saci_flash_mode` set.
///
/// debug_port_start exits SACI mode so the AHB-AP is accessible for normal
/// debug. Before flash commands can be sent the device must be reset back
/// into SACI mode.
fn reset_into_saci_mode(
    interface: &mut dyn ArmDebugInterface,
    saci_flash_mode: &AtomicBool,
) -> Result<(), ArmError> {
    tracing::info!("CC23xx/CC27xx: Re-initializing to enter SACI mode for flash programming");

    // After the reset the ROM waits in SACI. debug_port_start will see
    // saci_flash_mode == true and skip EXIT_SACI.
    saci_flash_mode.store(true, Ordering::SeqCst);
    saci::reset_device(interface);
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
            tracing::error!("CC23xx/CC27xx: Timeout waiting for SACI mode.");
            return Err(ArmError::Timeout);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Sends a SACI erase command and returns its result.
fn erase_command(
    interface: &mut dyn DapAccess,
    command: u32,
    parameters: u32,
) -> Result<SaciResult, ArmError> {
    let words = [saci::make_header(command, parameters), saci::cmd::FLASH_KEY];
    saci::send_words(interface, &words, Duration::from_millis(200))?;

    let response = saci::read_response(interface, Duration::from_secs(30))?;
    // The retained sector masks follow.
    for _ in 0..response >> 24 {
        let mask = saci::read_response(interface, Duration::from_millis(100))?;
        tracing::debug!("CC23xx/CC27xx: retained sectors {mask:#010x}");
    }
    Ok(SaciResult::from(((response >> 16) & 0xFF) as u8))
}

/// Erases the application sectors of a CC27xx with FLASH_ERASE_MAIN_APP.
///
/// The CCFG, the SCFG and the HSM firmware stay. After a reset, the ROM opens the AHB-AP.
#[derive(Debug)]
struct CC27xxEraseSequence {
    saci_flash_mode: Arc<AtomicBool>,
}

impl DebugEraseSequence for CC27xxEraseSequence {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        tracing::info!("CC27xx: erasing the application sectors with FLASH_ERASE_MAIN_APP");
        let result = reset_into_saci_mode(interface, &self.saci_flash_mode)
            .and_then(|()| erase_command(interface, saci::cmd::FLASH_ERASE_MAIN_APP, 0));

        // The ROM doesn't exit SACI in the session that erased the application: reset again.
        self.saci_flash_mode.store(false, Ordering::SeqCst);
        saci::reset_device(interface);
        interface.reinitialize()?;

        match result? {
            SaciResult::Success => Ok(()),
            result => Err(ArmError::Other(format!(
                "SACI FLASH_ERASE_MAIN_APP failed: {result:?}"
            ))),
        }
    }
}

impl CC23xxCC27xxFlashSequence {
    /// Reads the device's SCFG and compares it with the bytes that the download writes there.
    fn scfg_differs(session: &mut Session, loader: &FlashLoader) -> Result<bool, crate::Error> {
        let mut device = vec![0u8; SCFG_SIZE];
        session.core(0)?.read(SCFG_START, &mut device)?;
        for (address, data) in loader.data() {
            let end = address + data.len() as u64;
            let start = address.max(SCFG_START);
            let stop = end.min(SCFG_START + SCFG_SIZE as u64);
            if start >= stop {
                continue;
            }
            let image = &data[(start - address) as usize..(stop - address) as usize];
            let offset = (start - SCFG_START) as usize;
            if image != &device[offset..offset + image.len()] {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Whether the download writes into `start..start + size`.
    fn writes(loader: &FlashLoader, start: u64, size: u64) -> bool {
        loader
            .data()
            .any(|(address, data)| address < start + size && start < address + data.len() as u64)
    }

    /// Whether the CCFG passes a FLASH_VERIFY_CCFG_SECTOR check with `parameters`.
    fn verify_ccfg(interface: &mut dyn DapAccess, parameters: u32) -> Result<bool, ArmError> {
        let header = saci::make_header(saci::cmd::FLASH_VERIFY_CCFG_SECTOR, parameters);
        saci::send_words(interface, &[header, 0, 0, 0, 0], Duration::from_millis(200))?;
        let response = saci::read_response(interface, Duration::from_secs(5))?;
        Ok(SaciResult::from(((response >> 16) & 0xFF) as u8) == SaciResult::Success)
    }

    /// Whether the CCFG sector is blank.
    fn ccfg_is_blank(interface: &mut dyn DapAccess) -> Result<bool, ArmError> {
        Self::verify_ccfg(interface, BLANK_CHECK)
    }

    /// Whether the CCFG is valid: its embedded CRCs match, apart from the user record's.
    fn ccfg_is_valid(interface: &mut dyn DapAccess) -> Result<bool, ArmError> {
        Self::verify_ccfg(interface, SKIP_USER_RECORD)
    }

    fn check_erase(result: SaciResult) -> Result<(), crate::Error> {
        if result != SaciResult::Success {
            return Err(crate::Error::Other(format!(
                "SACI erase failed: {result:?}"
            )));
        }
        tracing::info!("CC23xx/CC27xx: Erase completed");
        Ok(())
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
    /// Pads data to exactly 512 words (2048 bytes) with 0xFF. The user record is written too:
    /// after the chip erase, it holds only what the image has there.
    fn program_ccfg(&self, interface: &mut dyn DapAccess, data: &[u8]) -> Result<(), ArmError> {
        // skip_user_rec = bit 0 of cmd_specific
        let header = saci::make_header(saci::cmd::FLASH_PROG_CCFG_SECTOR, 0);

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
        let data = &data[..data.len().min(SCFG_SIZE)];
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

/// Compares flash with the CRC checks of the SACI ROM.
///
/// For CCFG and SCFG, a match does not prove that the flash holds the image: the ROM checks the
/// CRCs in CCFG, and only 0xE4 bytes of SCFG. A standalone verify can pass on a different, valid
/// configuration. A download is not affected: these devices have no sector erase, so a compare
/// never skips programming. A read-back is not possible: SACI mode has no AHB-AP.
#[derive(Debug)]
pub struct SaciCompare;

impl FlashCompare for SaciCompare {
    fn name(&self) -> &str {
        "SACI"
    }

    fn cost(&self) -> CompareCost {
        CompareCost::Cheap
    }

    fn compare(
        &self,
        context: &mut CompareContext<'_>,
        _region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let interface = context
            .session()?
            .get_arm_interface()
            .map_err(|error| FlashError::Core(error.into()))?;
        for check in checks {
            let mut outcome = CompareOutcome::Match;
            for (address, data) in check.contiguous_ranges() {
                if !saci_verify(interface, address, &data).map_err(FlashError::Core)? {
                    outcome = CompareOutcome::Mismatch;
                    break;
                }
            }
            check.set_outcome(outcome);
        }

        Ok(())
    }
}

/// Checks `data` at `address` with a SACI verify command.
fn saci_verify(
    interface: &mut dyn ArmDebugInterface,
    address: u64,
    data: &[u8],
) -> Result<bool, crate::Error> {
    tracing::debug!(
        "CC23xx/CC27xx: Verifying {} bytes at 0x{:08X}",
        data.len(),
        address
    );

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
            _ => Err(
                ArmError::Other(format!("SACI FLASH_VERIFY_SCFG_SECTOR failed: {result:?}")).into(),
            ),
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
            _ => Err(
                ArmError::Other(format!("SACI FLASH_VERIFY_CCFG_SECTOR failed: {result:?}")).into(),
            ),
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
            _ => Err(
                ArmError::Other(format!("SACI FLASH_VERIFY_MAIN_SECTORS failed: {result:?}"))
                    .into(),
            ),
        }
    }
}

impl DebugFlashSequence for CC23xxCC27xxFlashSequence {
    fn prepare_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        let interface = session.get_arm_interface()?;

        // Always reset: a previous phase of this download may have left the device in SACI.
        reset_into_saci_mode(interface, &self.saci_flash_mode)?;
        Ok(())
    }

    /// On a CC27xx, the RAM flash algorithm flashes the download when the AHB-AP is open and the
    /// download keeps the device's SCFG, which the boot ROM can't write. Otherwise, SACI does,
    /// after a chip erase: the download must then hold the CCFG and the SCFG.
    fn flashes_download(
        &self,
        session: &mut Session,
        loader: &FlashLoader,
        _chip_erase: bool,
    ) -> Result<bool, crate::Error> {
        self.xcfg_pending.store(0, Ordering::SeqCst);

        let saci = if !self.cc27xx {
            true
        } else if !saci::read_device_status(session.get_arm_interface()?)?.ahb_ap_available() {
            tracing::warn!("CC27xx: no AHB-AP, flashing through SACI, which is slower");
            true
        } else if Self::scfg_differs(session, loader)? {
            tracing::warn!(
                "CC27xx: the download changes the SCFG, flashing through SACI, which is slower"
            );
            true
        } else {
            false
        };

        if saci
            && self.cc27xx
            && !(Self::writes(loader, CCFG_START, CCFG_SIZE)
                && Self::writes(loader, SCFG_START, SCFG_SIZE as u64))
        {
            return Err(crate::Error::Other(
                "CC27xx: flashing through SACI starts with a chip erase, which erases the CCFG \
                 and the SCFG, but the download doesn't hold both"
                    .to_string(),
            ));
        }

        if saci {
            let mut pending = 0;
            if Self::writes(loader, CCFG_START, CCFG_SIZE) {
                pending |= PENDING_CCFG;
            }
            if self.cc27xx && Self::writes(loader, SCFG_START, SCFG_SIZE as u64) {
                pending |= PENDING_SCFG;
            }
            self.xcfg_pending.store(pending, Ordering::SeqCst);
        }

        self.saci_download.store(saci, Ordering::SeqCst);
        Ok(saci)
    }

    /// The sectors that the CCFG retains are kept when possible. On a CC27xx, they hold the HSM
    /// firmware, which probe-rs can't provision again.
    fn erase_all(&self, session: &mut Session) -> Result<(), crate::Error> {
        let interface = session.get_arm_interface()?;

        // Retaining sectors needs a valid CCFG. With an invalid one, the ROM doesn't refuse the
        // option: it erases the sectors too.
        let result = if Self::ccfg_is_valid(interface)? {
            tracing::info!(
                "CC23xx/CC27xx: erasing the chip, keeping the sectors that the CCFG retains"
            );
            erase_command(interface, saci::cmd::FLASH_ERASE_CHIP, RETAIN_SECTORS)?
        } else if Self::ccfg_is_blank(interface)? {
            // Erasing the application never erases the HSM firmware. Parts without an HSM don't
            // have the command.
            tracing::info!("CC23xx/CC27xx: no CCFG, erasing the application sectors");
            match erase_command(interface, saci::cmd::FLASH_ERASE_MAIN_APP, 0)? {
                SaciResult::InvalidCmdId => {
                    tracing::info!("CC23xx/CC27xx: no FLASH_ERASE_MAIN_APP, erasing the chip");
                    erase_command(interface, saci::cmd::FLASH_ERASE_CHIP, 0)?
                }
                result => result,
            }
        } else {
            tracing::warn!("CC23xx/CC27xx: invalid CCFG, erasing the retained sectors too");
            erase_command(interface, saci::cmd::FLASH_ERASE_CHIP, 0)?
        };
        Self::check_erase(result)
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
            self.xcfg_pending.fetch_and(!PENDING_SCFG, Ordering::SeqCst);
        } else if address >= CCFG_START {
            self.program_ccfg(interface, data)?;
            self.xcfg_pending.fetch_and(!PENDING_CCFG, Ordering::SeqCst);
        } else if data.iter().all(|&byte| byte == 0xFF) {
            // A SACI download starts with an erase, which leaves the page blank.
            tracing::debug!("CC23xx/CC27xx: page at {address:#010x} is blank, not programmed");
        } else {
            self.program_main(interface, address, data)?;
        }
        Ok(())
    }

    fn supports_sector_erase(&self) -> bool {
        false
    }

    fn finish_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        // The boot ROM only leaves SACI with a valid CCFG and SCFG. A later phase of this download
        // programs them, and resets into SACI again.
        if self.xcfg_pending.load(Ordering::SeqCst) != 0 {
            tracing::debug!("CC23xx/CC27xx: CCFG or SCFG not programmed yet, staying in SACI");
            return Ok(());
        }

        // The session is reused between the flash operation and post-flash debug
        // access, so the device must be out of SACI mode before we return.
        //
        // Sequence (matches OpenOCD cc27xx reset_halt after programming):
        // 1. Clear saci_flash_mode so the next debug_port_start exits SACI.
        // 2. Reset into SACI and reinitialize, so debug_port_start sends
        //    EXIT_SACI_HALT (device boots, AHB-AP available).
        tracing::info!("CC23xx/CC27xx: Flash complete, exiting SACI for normal debug access");

        // Clear the flag BEFORE reinitialize so debug_port_start sends
        // EXIT_SACI_HALT instead of staying in SACI mode.
        self.saci_flash_mode.store(false, Ordering::SeqCst);

        let interface = session.get_arm_interface()?;
        saci::reset_device(interface);
        interface.reinitialize()?;

        Ok(())
    }
}
