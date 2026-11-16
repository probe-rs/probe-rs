//! Sequences for CC23xx/CC27xx devices.
use std::ops::{DerefMut, Range};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::saci;
use super::saci::{DeviceStatusRegister, SaciResult};
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
    ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence, cortex_m_core_start,
};
use crate::architecture::arm::traits::DebugPortWire;
use crate::architecture::arm::{ArmError, FullyQualifiedApAddress};
use crate::config::{MemoryRegion, NvmRegion, Target};
use crate::flashing::{
    CompareContext, CompareCost, CompareOutcome, DebugFlashSequence, FlashCompare, FlashError,
    FlashLoader, SectorCheck,
};
use crate::probe::{DebugProbeError, WireProtocol};
use probe_rs_target::CoreType;

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
    /// AHB-AP state as of the last debug_port_start.
    ahb_ap_available: AtomicBool,
    /// Set by reset_system when no halt was requested, so debug_port_start
    /// boots the application instead of halting in the ROM.
    run_after_reset: AtomicBool,
    /// Whether the current download goes through SACI rather than the RAM flash algorithm.
    ///
    /// Set by the flash sequence for each download.
    is_saci_download: Arc<AtomicBool>,
    /// Whether the current download has provisioned the HSM firmware.
    ///
    /// Set by the flash sequence, read by the SACI compare.
    hsm_provisioned: Arc<AtomicBool>,
}

/// Flash memory region types for CC23xx/CC27xx devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashRegion {
    /// Main application flash.
    Main,
    /// Customer Configuration (CCFG) sector.
    Ccfg,
    /// Security Configuration (SCFG) sector, on the parts that have one.
    Scfg,
    /// HSM firmware, on the parts that have one. The boot ROM read-protects it.
    Hsm,
}

impl FlashRegion {
    /// The kind of a memory map region, from its name.
    fn from_nvm_region(region: &NvmRegion) -> Self {
        match region.name.as_deref() {
            Some(CCFG_REGION) => Self::Ccfg,
            Some(SCFG_REGION) => Self::Scfg,
            Some(HSM_REGION) => Self::Hsm,
            _ => Self::Main,
        }
    }
}

/// The name of the CCFG region in the target description.
const CCFG_REGION: &str = "CCFG";
/// The name of the SCFG region in the target description.
const SCFG_REGION: &str = "SCFG";
/// The name of the HSM firmware region in the target description.
const HSM_REGION: &str = "HSM";

/// The configuration sectors of a device, from the named regions of its memory map.
#[derive(Debug)]
struct ConfigSectors {
    ccfg: Range<u64>,
    /// `None` on parts without an SCFG.
    scfg: Option<Range<u64>>,
    /// `None` on parts without an HSM.
    hsm: Option<Range<u64>>,
}

impl ConfigSectors {
    fn try_from_target(target: &Target) -> Result<Self, crate::Error> {
        let region = |name| {
            target.memory_map.iter().find_map(|region| match region {
                MemoryRegion::Nvm(nvm) if nvm.name.as_deref() == Some(name) => {
                    Some(nvm.range.clone())
                }
                _ => None,
            })
        };
        let ccfg = region(CCFG_REGION).ok_or_else(|| {
            crate::Error::Other(format!("{}: no {CCFG_REGION} region", target.name))
        })?;
        Ok(Self {
            ccfg,
            scfg: region(SCFG_REGION),
            hsm: region(HSM_REGION),
        })
    }

    /// The configuration sectors, with their region names.
    fn by_name(&self) -> impl Iterator<Item = (&'static str, &Range<u64>)> {
        std::iter::once((CCFG_REGION, &self.ccfg))
            .chain(self.scfg.as_ref().map(|scfg| (SCFG_REGION, scfg)))
    }

    /// The kind of sector that holds `address`.
    fn flash_region_at(&self, address: u64) -> FlashRegion {
        if self.ccfg.contains(&address) {
            FlashRegion::Ccfg
        } else if self
            .scfg
            .as_ref()
            .is_some_and(|scfg| scfg.contains(&address))
        {
            FlashRegion::Scfg
        } else if self.hsm.as_ref().is_some_and(|hsm| hsm.contains(&address)) {
            FlashRegion::Hsm
        } else {
            FlashRegion::Main
        }
    }
}

impl CC23xxCC27xx {
    /// Create the sequencer for the CC23xx/CC27xx family.
    pub fn create(name: String) -> Arc<Self> {
        Arc::new(Self {
            _name: name,
            boot_loop: AtomicBool::new(false),
            saci_flash_mode: Arc::new(AtomicBool::new(false)),
            ahb_ap_available: AtomicBool::new(false),
            run_after_reset: AtomicBool::new(false),
            is_saci_download: Arc::new(AtomicBool::new(true)),
            hsm_provisioned: Arc::new(AtomicBool::new(false)),
        })
    }

    fn is_in_boot_loop(&self) -> bool {
        self.boot_loop.load(Ordering::SeqCst)
    }

    /// Leave SACI and wait for the ROM to act on the exit command.
    ///
    /// Which command is sent depends on `run_after_reset`, set by reset_system:
    ///
    /// - Run requested (reset without halt): send BLDR_APP_EXIT_SACI_RUN. The
    ///   ROM boots the application, which is done once the AHB-AP is available.
    /// - Halt requested, or a plain connect: send DEBUG_EXIT_SACI_HALT. The ROM
    ///   stops in its wait loop before running the application, which is done
    ///   once the boot status reports that wait loop. debug_core_start then
    ///   halts the core and releases it.
    ///
    /// Returns the final device status. If the wanted state is not reached in
    /// time the last status is returned instead of an error, so the caller can
    /// inspect it and decide what to report.
    fn exit_saci(&self, interface: &mut dyn DapAccess) -> Result<DeviceStatusRegister, ArmError> {
        // In practice, this should take less time and bails early, timeout only prevents forever loop
        const EXIT_SACI_TIMEOUT: Duration = Duration::from_secs(2);

        let status = if self.run_after_reset.load(Ordering::SeqCst) {
            saci::send_command(interface, saci::cmd::BLDR_APP_EXIT_SACI_RUN)?;
            saci::wait_for_device_status(
                interface,
                EXIT_SACI_TIMEOUT,
                DeviceStatusRegister::ahb_ap_available,
            )
        } else {
            saci::send_command(interface, saci::cmd::DEBUG_EXIT_SACI_HALT)?;
            saci::wait_for_device_status(
                interface,
                EXIT_SACI_TIMEOUT,
                DeviceStatusRegister::in_wait_loop,
            )
        };

        // A timeout is not fatal here: the caller checks the status it gets back.
        let status = status.or_else(|_| saci::read_device_status(interface))?;

        // The ROM refused the command and answered it. Read the answer, or the next SACI command
        // would read it as its own.
        if !status.ahb_ap_available() {
            match saci::read_response(interface, Duration::from_millis(100)) {
                Ok(response) => tracing::debug!(
                    "CC23xx/CC27xx: exit from SACI refused: {:?}",
                    SaciResult::from(((response >> 16) & 0xFF) as u8)
                ),
                Err(error) => {
                    tracing::debug!("CC23xx/CC27xx: exit from SACI not answered: {error}")
                }
            }
        }
        Ok(status)
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
        self.run_after_reset
            .store(!demcr.vc_corereset(), Ordering::SeqCst);
        let reconnected = interface.reinitialize();
        self.run_after_reset.store(false, Ordering::SeqCst);
        reconnected?;
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
        // Right after a reset the ROM may not have reached SACI yet.
        let mut device_status = saci::wait_for_device_status(
            interface,
            Duration::from_millis(100),
            DeviceStatusRegister::has_reached_saci,
        )?;

        if !device_status.ahb_ap_available() {
            if self.saci_flash_mode.load(Ordering::SeqCst) {
                // Flash programming is in progress; stay in SACI mode so the
                // ROM flash handler remains active for subsequent SACI commands.
                tracing::info!(
                    "CC23xx/CC27xx: debug_port_start in flash mode, leaving SACI active"
                );
                self.boot_loop.store(false, Ordering::SeqCst);
                self.ahb_ap_available.store(false, Ordering::SeqCst);
                return Ok(());
            }

            // Normal debug session: exit SACI so the AHB-AP becomes accessible.
            device_status = self.exit_saci(interface)?;
            if !device_status.ahb_ap_available() {
                tracing::warn!(
                    "CC23xx/CC27xx: Device stayed in SACI mode (no valid image or debug locked), only flashing is available"
                );
            }
        }

        // Set on every connect, so a stale flag never makes debug_core_start
        // halt a core that was booted.
        self.boot_loop
            .store(device_status.in_wait_loop(), Ordering::SeqCst);
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
            is_saci_download: Arc::clone(&self.is_saci_download),
            hsm_provisioned: Arc::clone(&self.hsm_provisioned),
            hsm_download: AtomicBool::new(false),
            xcfgs_pending: AtomicU8::new(0),
        }))
    }

    fn flash_compare(&self) -> Option<Arc<dyn FlashCompare>> {
        if !self.is_saci_download.load(Ordering::SeqCst) {
            return None;
        }
        Some(Arc::new(SaciCompare {
            hsm_provisioned: Arc::clone(&self.hsm_provisioned),
        }))
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        Some(Arc::new(MainAppEraseSequence {
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
        // Refuse anything besides SWD
        if interface.active_protocol() != Some(WireProtocol::Swd) {
            return Err(ArmDebugSequenceError::SequenceSpecific(
                "The CC23xx/CC27xx family supports SWD only".into(),
            )
            .into());
        }

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

/// The header of TI's HSM firmware images: the payload follows the header.
const HSM_IMAGE_HEADER_SIZE: usize = 0x410;
/// The magic of TI's HSM firmware images.
const HSM_IMAGE_MAGIC: (usize, &[u8; 4]) = (0x404, b"FWr\0");
/// The offset of the payload size in TI's HSM firmware images.
const HSM_IMAGE_PAYLOAD_SIZE: usize = 0x408;

/// Returns the HSM firmware image at the start of `data`, without the padding after it.
fn hsm_image(data: &[u8]) -> Result<&[u8], ArmError> {
    let (offset, magic) = HSM_IMAGE_MAGIC;
    if data.len() < HSM_IMAGE_HEADER_SIZE || &data[offset..offset + 4] != magic {
        return Err(ArmError::Other(
            "CC23xx/CC27xx: the data in the HSM region is not an HSM firmware image".to_string(),
        ));
    }
    let payload = u32::from_le_bytes(
        data[HSM_IMAGE_PAYLOAD_SIZE..HSM_IMAGE_PAYLOAD_SIZE + 4]
            .try_into()
            .unwrap(),
    ) as usize;
    let size = HSM_IMAGE_HEADER_SIZE + payload;
    if size > data.len() || !size.is_multiple_of(4) {
        return Err(ArmError::Other(format!(
            "CC23xx/CC27xx: invalid HSM firmware image size {size:#x}"
        )));
    }
    Ok(&data[..size])
}

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
    is_saci_download: Arc<AtomicBool>,
    /// Shared with CC23xxCC27xx: whether the current download has provisioned the HSM firmware.
    hsm_provisioned: Arc<AtomicBool>,
    /// Whether the current download is an HSM firmware image, which goes alone.
    hsm_download: AtomicBool,
    /// The configuration sectors that the current download still has to program through SACI:
    /// [`PENDING_CCFG`] and [`PENDING_SCFG`].
    xcfgs_pending: AtomicU8,
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

/// Erases the application sectors with FLASH_ERASE_MAIN_APP.
///
/// The CCFG, the SCFG and the HSM firmware stay. After a reset, the ROM opens the AHB-AP. On parts
/// without the command, erases the chip as the SACI download path does.
#[derive(Debug)]
struct MainAppEraseSequence {
    saci_flash_mode: Arc<AtomicBool>,
}

impl DebugEraseSequence for MainAppEraseSequence {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        tracing::info!("CC23xx/CC27xx: erasing the application sectors with FLASH_ERASE_MAIN_APP");
        let result = reset_into_saci_mode(interface, &self.saci_flash_mode)
            .and_then(|()| saci::erase_command(interface, saci::cmd::FLASH_ERASE_MAIN_APP, 0))
            .and_then(|result| match result {
                SaciResult::InvalidCmdId => {
                    tracing::info!("CC23xx/CC27xx: no FLASH_ERASE_MAIN_APP");
                    saci::erase_chip(interface)
                }
                result => Ok(result),
            });

        // The ROM doesn't exit SACI in the session that erased the application: reset again.
        self.saci_flash_mode.store(false, Ordering::SeqCst);
        saci::reset_device(interface);
        interface.reinitialize()?;

        match result? {
            SaciResult::Success => Ok(()),
            result => Err(ArmError::Other(format!("SACI erase failed: {result:?}"))),
        }
    }
}

/// Whether the data `blocks`, as (address, data) pairs, cover all of `range`.
/// VIMS WEPRA, WEPRB0, WEPRB1 and WEPRAUX: the flash write/erase protection, sticky 0. A 0 bit
/// protects.
const VIMS_WEPR: u64 = 0x4002_4410;
/// The WEPRAUX bit of the CCFG sector.
const WEPRAUX_CCFG: u32 = 1 << 0;
const MAIN_SECTOR_SIZE: u64 = 0x800;

/// Whether MAIN sector `sector` is write-protected, as driverlib's `FlashGetProtection` reads it:
/// WEPRA protects sectors 0 to 31 one by one, WEPRB0 and WEPRB1 groups of 8 sectors.
fn main_sector_protected(wepr: &[u32; 4], sector: u64) -> bool {
    let (register, bit) = match sector {
        0..32 => (wepr[0], sector),
        32..256 => (wepr[1], sector / 8),
        256..512 => (wepr[2], sector / 8 % 32),
        _ => return false,
    };
    register & (1 << bit) == 0
}

fn covers<'a>(blocks: impl Iterator<Item = (u64, &'a [u8])>, range: &Range<u64>) -> bool {
    let mut blocks: Vec<Range<u64>> = blocks
        .map(|(address, data)| address..address + data.len() as u64)
        .filter(|block| block.start < range.end && range.start < block.end)
        .collect();
    blocks.sort_by_key(|block| block.start);

    let mut covered = range.start;
    for block in blocks {
        if block.start > covered {
            return false;
        }
        covered = covered.max(block.end);
    }
    covered >= range.end
}

impl CC23xxCC27xxFlashSequence {
    /// Reads `range` from the device and compares it with the bytes that the download writes there.
    fn differs(
        session: &mut Session,
        loader: &FlashLoader,
        range: &Range<u64>,
    ) -> Result<bool, crate::Error> {
        let mut device = vec![0u8; (range.end - range.start) as usize];
        session.core(0)?.read(range.start, &mut device)?;
        for (address, data) in loader.data() {
            let end = address + data.len() as u64;
            let start = address.max(range.start);
            let stop = end.min(range.end);
            if start >= stop {
                continue;
            }
            let image = &data[(start - address) as usize..(stop - address) as usize];
            let offset = (start - range.start) as usize;
            if image != &device[offset..offset + image.len()] {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The first sector that the download erases, but which the boot ROM write-protected, as the
    /// FCFG and the CCFG ask. The RAM flash algorithm would fail there, possibly after it erased
    /// other sectors. The CCFG sector counts only when the download changes it: the RAM flash
    /// algorithm skips an unchanged one.
    fn write_protected(
        session: &mut Session,
        loader: &FlashLoader,
        sectors: &ConfigSectors,
    ) -> Result<Option<String>, crate::Error> {
        let mut wepr = [0u32; 4];
        session.core(0)?.read_32(VIMS_WEPR, &mut wepr)?;
        tracing::debug!("CC23xx/CC27xx: VIMS write/erase protection {wepr:08x?}");

        if wepr[3] & WEPRAUX_CCFG == 0
            && Self::writes(loader, &sectors.ccfg)
            && Self::differs(session, loader, &sectors.ccfg)?
        {
            return Ok(Some("CCFG".to_string()));
        }

        let mut erased: Vec<u64> = loader
            .data()
            .filter(|&(address, _)| sectors.flash_region_at(address) == FlashRegion::Main)
            .flat_map(|(address, data)| {
                let end = address + data.len() as u64;
                address / MAIN_SECTOR_SIZE..end.div_ceil(MAIN_SECTOR_SIZE)
            })
            .collect();
        erased.sort_unstable();
        erased.dedup();
        Ok(erased
            .into_iter()
            .find(|&sector| main_sector_protected(&wepr, sector))
            .map(|sector| format!("MAIN sector {sector} at {:#x}", sector * MAIN_SECTOR_SIZE)))
    }

    /// Whether the download writes into `range`.
    fn writes(loader: &FlashLoader, range: &Range<u64>) -> bool {
        loader
            .data()
            .any(|(address, data)| address < range.end && range.start < address + data.len() as u64)
    }

    /// Whether the download writes all of `range`.
    fn holds(loader: &FlashLoader, range: &Range<u64>) -> bool {
        covers(loader.data(), range)
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
    /// Pads data to the whole sector with 0xFF. The user record is written too: after the chip
    /// erase, it holds only what the image has there.
    fn program_ccfg(
        &self,
        interface: &mut dyn DapAccess,
        data: &[u8],
        size: usize,
    ) -> Result<(), ArmError> {
        // skip_user_rec = bit 0 of cmd_specific
        let header = saci::make_header(saci::cmd::FLASH_PROG_CCFG_SECTOR, 0);

        let mut padded = vec![0xFFu8; size];
        let copy_len = data.len().min(size);
        padded[..copy_len].copy_from_slice(&data[..copy_len]);

        let mut words = vec![header, saci::cmd::FLASH_KEY];
        words.extend(saci::pack_words(&padded, 0xFF));

        saci::send_words(interface, &words, Duration::from_millis(200))?;
        let response = saci::read_response(interface, Duration::from_secs(5))?;
        saci::check_result(response, "FLASH_PROG_CCFG_SECTOR")?;
        Ok(())
    }

    /// Program the SCFG sector using FLASH_PROG_SCFG_SECTOR (0x1A).
    fn program_scfg(
        &self,
        interface: &mut dyn DapAccess,
        data: &[u8],
        size: usize,
    ) -> Result<(), ArmError> {
        let data = &data[..data.len().min(size)];
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
///
/// The HSM firmware can't be read back. Before it is provisioned, its sectors count as changed.
/// After the download provisioned it, they match if the HSM firmware runs.
#[derive(Debug)]
pub struct SaciCompare {
    /// Whether the current download has provisioned the HSM firmware.
    hsm_provisioned: Arc<AtomicBool>,
}

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
        region: &NvmRegion,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let kind = FlashRegion::from_nvm_region(region);
        if kind == FlashRegion::Hsm {
            return self.compare_hsm(context, checks);
        }
        let interface = context
            .session()?
            .get_arm_interface()
            .map_err(|error| FlashError::Core(error.into()))?;
        for check in checks {
            let mut outcome = CompareOutcome::Match;
            for (address, data) in check.contiguous_ranges() {
                if !saci_verify(interface, kind, address, &data).map_err(FlashError::Core)? {
                    outcome = CompareOutcome::Mismatch;
                    break;
                }
            }
            check.set_outcome(outcome);
        }

        Ok(())
    }
}

/// The CRCs that a CCFG image holds: those of the boot configuration, the main part, the user
/// record and the debug configuration. `None` unless `data` is the whole sector.
fn ccfg_crcs(data: &[u8]) -> Option<[u32; 4]> {
    const CCFG_SIZE: usize = 0x800;
    const CRC_OFFSETS: [usize; 4] = [0x00C, 0x74C, 0x7CC, 0x7FC];
    if data.len() != CCFG_SIZE {
        return None;
    }
    Some(CRC_OFFSETS.map(|offset| u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())))
}

impl SaciCompare {
    fn compare_hsm(
        &self,
        context: &mut CompareContext<'_>,
        checks: &mut [SectorCheck<'_>],
    ) -> Result<(), FlashError> {
        let outcome = if self.hsm_provisioned.load(Ordering::SeqCst) {
            let interface = context
                .session()?
                .get_arm_interface()
                .map_err(|error| FlashError::Core(error.into()))?;
            if saci::hsm_runs(interface).map_err(|error| FlashError::Core(error.into()))? {
                CompareOutcome::Match
            } else {
                CompareOutcome::Mismatch
            }
        } else {
            tracing::warn!(
                "CC23xx/CC27xx: the HSM firmware can't be read back, it is checked only right \
                 after it is provisioned"
            );
            CompareOutcome::Mismatch
        };

        for check in checks {
            check.set_outcome(outcome);
        }
        Ok(())
    }
}

/// Checks `data` at `address` with a SACI verify command.
fn saci_verify(
    interface: &mut dyn ArmDebugInterface,
    kind: FlashRegion,
    address: u64,
    data: &[u8],
) -> Result<bool, crate::Error> {
    tracing::debug!(
        "CC23xx/CC27xx: Verifying {} bytes at 0x{:08X}",
        data.len(),
        address
    );

    if kind == FlashRegion::Scfg {
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
    } else if kind == FlashRegion::Ccfg {
        // FLASH_VERIFY_CCFG_SECTOR (0x11): check_exp_crcs=1, skip_user_rec=1, then the expected
        // CRCs of the boot configuration, the main part, the user record and the debug
        // configuration. The image's CCFG holds them at the end of each part. Without them, the
        // ROM only checks that the device's CCFG matches its own CRCs.
        let Some(expected) = ccfg_crcs(data) else {
            return Ok(false);
        };
        let header = saci::make_header(saci::cmd::FLASH_VERIFY_CCFG_SECTOR, 0x0003);
        let words = [header, expected[0], expected[1], expected[2], expected[3]];
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

        // The device is still in SACI if a previous phase of this download left it there, or if it
        // couldn't leave SACI when the session attached. HSM_FW_PROVISION must be the first command
        // of a SACI session.
        if self.hsm_download.load(Ordering::SeqCst)
            || matches!(saci::read_device_status(interface), Ok(s) if s.ahb_ap_available())
        {
            reset_into_saci_mode(interface, &self.saci_flash_mode)?;
        } else {
            self.saci_flash_mode.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// When the target has a RAM flash algorithm, it flashes the download if the AHB-AP is open,
    /// the download keeps the device's SCFG, which the boot ROM can't write, and it writes no
    /// write-protected sector. Otherwise, or for a chip erase, SACI does, after a chip erase: the
    /// download must then hold the configuration sectors.
    fn flashes_download(
        &self,
        session: &mut Session,
        loader: &FlashLoader,
        chip_erase: bool,
    ) -> Result<bool, crate::Error> {
        self.xcfgs_pending.store(0, Ordering::SeqCst);
        self.hsm_provisioned.store(false, Ordering::SeqCst);

        let sectors = ConfigSectors::try_from_target(session.target())?;
        // A partly written configuration sector is invalid, and the ROM then stays in SACI.
        for (name, range) in sectors.by_name() {
            if Self::writes(loader, range) && !Self::holds(loader, range) {
                return Err(crate::Error::Other(format!(
                    "CC23xx/CC27xx: the download writes part of the {name} sector, but not all of it"
                )));
            }
        }

        let hsm = sectors
            .hsm
            .as_ref()
            .is_some_and(|hsm| Self::writes(loader, hsm));
        self.hsm_download.store(hsm, Ordering::SeqCst);
        if let Some(range) = sectors.hsm.as_ref().filter(|_| hsm) {
            if loader.data().any(|(address, data)| {
                address < range.start || address + data.len() as u64 > range.end
            }) {
                return Err(crate::Error::Other(
                    "CC23xx/CC27xx: an HSM firmware image must be downloaded on its own"
                        .to_string(),
                ));
            }
            tracing::info!("CC23xx/CC27xx: HSM firmware image, through SACI");
            self.is_saci_download.store(true, Ordering::SeqCst);
            return Ok(true);
        }

        let ram_algorithm = session
            .target()
            .flash_algorithms
            .iter()
            .any(|algorithm| !algorithm.instructions.is_empty());

        let saci = if !ram_algorithm || chip_erase {
            true
        } else if !saci::read_device_status(session.get_arm_interface()?)?.ahb_ap_available() {
            tracing::warn!("CC23xx/CC27xx: no AHB-AP, flashing through SACI, which is slower");
            true
        } else if let Some(scfg) = &sectors.scfg
            && Self::differs(session, loader, scfg)?
        {
            tracing::warn!(
                "CC23xx/CC27xx: the download changes the SCFG, flashing through SACI, which is \
                 slower"
            );
            true
        } else if let Some(sector) = Self::write_protected(session, loader, &sectors)? {
            tracing::warn!(
                "CC23xx/CC27xx: the download writes the write-protected {sector}, flashing \
                 through SACI, which is slower"
            );
            true
        } else {
            false
        };

        if saci {
            if !sectors
                .by_name()
                .all(|(_, range)| Self::holds(loader, range))
            {
                return Err(crate::Error::Other(
                    "CC23xx/CC27xx: flashing through SACI starts with a chip erase, which erases \
                     the configuration sectors, but the download doesn't hold them all"
                        .to_string(),
                ));
            }

            let mut pending = PENDING_CCFG;
            if sectors.scfg.is_some() {
                pending |= PENDING_SCFG;
            }
            self.xcfgs_pending.store(pending, Ordering::SeqCst);
        }

        self.is_saci_download.store(saci, Ordering::SeqCst);
        Ok(saci)
    }

    /// The sectors that the CCFG retains are kept when possible. On parts with an HSM, they hold
    /// its firmware, which probe-rs can't provision again.
    fn erase_all(&self, session: &mut Session) -> Result<(), crate::Error> {
        // Provisioning the HSM firmware needs no erase, and must be alone in its SACI session.
        if self.hsm_download.load(Ordering::SeqCst) {
            tracing::info!("CC23xx/CC27xx: HSM firmware image, no erase");
            return Ok(());
        }

        let result = saci::erase_chip(session.get_arm_interface()?)?;
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

        let sectors = ConfigSectors::try_from_target(session.target())?;
        let interface = session.get_arm_interface()?;

        match sectors.flash_region_at(address) {
            FlashRegion::Hsm => {
                saci::provision_hsm(interface, hsm_image(data)?)?;
                self.hsm_provisioned.store(true, Ordering::SeqCst);
            }
            FlashRegion::Scfg => {
                let scfg = sectors.scfg.unwrap_or_default();
                self.program_scfg(interface, data, (scfg.end - scfg.start) as usize)?;
                self.xcfgs_pending
                    .fetch_and(!PENDING_SCFG, Ordering::SeqCst);
            }
            FlashRegion::Ccfg => {
                let ccfg = sectors.ccfg;
                self.program_ccfg(interface, data, (ccfg.end - ccfg.start) as usize)?;
                self.xcfgs_pending
                    .fetch_and(!PENDING_CCFG, Ordering::SeqCst);
            }
            FlashRegion::Main if data.iter().all(|&byte| byte == 0xFF) => {
                // A SACI download starts with an erase, which leaves the page blank.
                tracing::debug!("CC23xx/CC27xx: page at {address:#010x} is blank, not programmed");
            }
            FlashRegion::Main => self.program_main(interface, address, data)?,
        }
        Ok(())
    }

    /// The HSM region needs no erase: for an HSM firmware image, no chip erase either.
    fn supports_sector_erase(&self) -> bool {
        self.hsm_download.load(Ordering::SeqCst)
    }

    fn erase_sector(&self, _session: &mut Session, _address: u64) -> Result<(), crate::Error> {
        if self.hsm_download.load(Ordering::SeqCst) {
            return Ok(());
        }
        Err(crate::Error::NotImplemented("sector erase"))
    }

    fn finish_flash(&self, session: &mut Session) -> Result<(), crate::Error> {
        // The boot ROM only leaves SACI with a valid CCFG and SCFG. A later phase of this download
        // programs them, and resets into SACI again.
        if self.xcfgs_pending.load(Ordering::SeqCst) != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Registry;

    #[test]
    fn every_target_has_a_ccfg_region() {
        let registry = Registry::from_builtin_families();
        let family = registry
            .families()
            .iter()
            .find(|family| family.name == "CC23XX CC27XX Series")
            .unwrap();
        for chip in family.variants() {
            let target = registry.get_target_by_name(&chip.name).unwrap();
            if let Err(error) = ConfigSectors::try_from_target(&target) {
                panic!("{error}");
            }
        }
    }

    #[test]
    fn main_sector_protection() {
        // A CC2755P10 with the HSM firmware, which protects sectors 464 to 511.
        let wepr = [0xffff_ffff, 0xffff_ffff, 0x03ff_ffff, 0x0000_0001];
        assert!(!main_sector_protected(&wepr, 0));
        assert!(!main_sector_protected(&wepr, 463));
        assert!(main_sector_protected(&wepr, 464));
        assert!(main_sector_protected(&wepr, 511));

        let wepr = [!(1 << 3), !(1 << 5), !(1 << 0), 0];
        assert!(main_sector_protected(&wepr, 3));
        assert!(!main_sector_protected(&wepr, 4));
        assert!(main_sector_protected(&wepr, 40));
        assert!(main_sector_protected(&wepr, 47));
        assert!(!main_sector_protected(&wepr, 48));
        assert!(main_sector_protected(&wepr, 256));
        assert!(!main_sector_protected(&wepr, 264));
    }

    #[test]
    fn covers_needs_every_byte() {
        let data = [0u8; 0x400];
        let range = 0x1000..0x1800;
        let split = [(0x1000, &data[..]), (0x1400, &data[..])];
        assert!(covers(split.into_iter(), &range));
        assert!(covers(
            [(0x1400, &data[..]), (0x1000, &data[..])].into_iter(),
            &range
        ));
        assert!(!covers([(0x1000, &data[..])].into_iter(), &range));
        assert!(!covers(
            [(0x1000, &data[..0x3ff]), (0x1400, &data[..])].into_iter(),
            &range
        ));
        assert!(!covers(std::iter::empty(), &range));
        let large = [0u8; 0x1000];
        assert!(covers([(0x800, &large[..])].into_iter(), &range));
    }
}
