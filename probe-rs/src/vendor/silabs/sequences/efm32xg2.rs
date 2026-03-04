//! Sequences for Silicon Labs EFM32 Series 2 chips

use std::{
    fmt::Debug,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::Chip;

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        ap::{ApRegister, CSW},
        core::armv8m::{Aircr, Demcr, Dhcsr},
        memory::ArmMemoryInterface,
        sequences::{
            ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence, DebugLockSequence,
            LockLevel, cortex_m_wait_for_reset,
        },
    },
    core::MemoryMappedRegister,
    session::MissingPermissions,
};

// DCI register addresses (accessed via MEM-AP at AP index 1)
const DCI_WDATA: u64 = 0x1000;
const DCI_RDATA: u64 = 0x1004;
const DCI_STATUS: u64 = 0x1008;

// DCI_STATUS bits
const DCI_STATUS_WPENDING: u32 = 1 << 0;
const DCI_STATUS_RDATAVALID: u32 = 1 << 8;

// SE command words
const SE_CMD_HEADER_LEN: u32 = 0x08;
const SE_CMD_DEVICE_ERASE: u32 = 0x430F_0000;
const SE_CMD_APPLY_LOCK: u32 = 0x430C_0000;
const SE_CMD_STATUS_QUERY: u32 = 0xFE01_0000;

// SE status response debug lock bits
const SE_STATUS_DEVICE_ERASE_ENABLED: u32 = 1 << 1;
const SE_STATUS_HW_DEBUG_LOCK_ACTIVE: u32 = 1 << 5;

/// Timeout for DCI status polling
const DCI_POLL_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for device erase completion
const DCI_ERASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Poll `condition` every `interval` until it returns `Ok(true)` or `timeout` elapses.
/// Returns `Ok(true)` if the condition was met, `Ok(false)` on timeout.
fn poll_until(
    timeout: Duration,
    interval: Duration,
    mut condition: impl FnMut() -> Result<bool, ArmError>,
) -> Result<bool, ArmError> {
    let start = Instant::now();
    loop {
        if condition()? {
            return Ok(true);
        }
        if start.elapsed() >= timeout {
            return Ok(false);
        }
        thread::sleep(interval);
    }
}

/// Result of querying the SE for debug lock status.
enum SeDebugLockStatus {
    /// Debug port is not locked.
    Unlocked,
    /// Locked, but device erase is available to unlock.
    LockedEraseEnabled,
    /// Locked permanently — device erase is disabled.
    PermanentlyLocked,
}

/// The sequence handle for the EFM32 Series 2 family.
///
/// Uses a breakpoint on the reset vector for the reset catch.
#[derive(Debug, Clone)]
pub struct EFM32xG2 {
    flash_base_addr: u64,
    use_msc_erase: bool,
}

impl EFM32xG2 {
    /// Create a sequence handle for the EFM32xG2
    pub fn create(chip: &Chip) -> Arc<dyn ArmDebugSequence> {
        let is_series_2c3 = chip.name.starts_with("EFR32FG23")
            || chip.name.starts_with("EFR32MG24")
            || chip.name.starts_with("EFR32PG26");

        let flash_base_addr = if is_series_2c3 { 0x0800_0000 } else { 0 };

        Arc::new(Self {
            flash_base_addr,
            use_msc_erase: is_series_2c3,
        })
    }

    /// Poll DCI_STATUS until WPENDING (bit 0) is clear and RDATAVALID (bit 8) is not set.
    fn dci_wait_ready(mem: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let ready = poll_until(DCI_POLL_TIMEOUT, Duration::from_millis(10), || {
            let status = mem.read_word_32(DCI_STATUS)?;
            Ok(status & DCI_STATUS_WPENDING == 0 && status & DCI_STATUS_RDATAVALID == 0)
        })?;
        if !ready {
            return Err(ArmError::Timeout);
        }
        Ok(())
    }

    /// Poll DCI_STATUS until RDATAVALID (bit 8) is set.
    fn dci_wait_rdatavalid(mem: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let ready = poll_until(DCI_POLL_TIMEOUT, Duration::from_millis(10), || {
            let status = mem.read_word_32(DCI_STATUS)?;
            Ok(status & DCI_STATUS_RDATAVALID != 0)
        })?;
        if !ready {
            return Err(ArmError::Timeout);
        }
        Ok(())
    }

    /// Write a command word to DCI_WDATA, waiting for the interface to be ready first.
    fn dci_write_cmd(mem: &mut dyn ArmMemoryInterface, word: u32) -> Result<(), ArmError> {
        Self::dci_wait_ready(mem)?;
        mem.write_word_32(DCI_WDATA, word)
    }

    /// Read a response word from DCI_RDATA, waiting for RDATAVALID first.
    fn dci_read_response(mem: &mut dyn ArmMemoryInterface) -> Result<u32, ArmError> {
        Self::dci_wait_rdatavalid(mem)?;
        mem.read_word_32(DCI_RDATA)
    }

    /// Check whether the device is unlocked by reading CSW.DeviceEn on the given AP.
    fn is_device_unlocked(
        interface: &mut dyn ArmDebugInterface,
        ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let csw: CSW = interface
            .read_raw_ap_register(ap, CSW::ADDRESS)?
            .try_into()?;
        Ok(csw.DeviceEn)
    }

    /// Query the SE via DCI for the current debug lock status.
    fn se_query_debug_lock_status(
        mem: &mut dyn ArmMemoryInterface,
    ) -> Result<SeDebugLockStatus, ArmError> {
        Self::dci_write_cmd(mem, SE_CMD_HEADER_LEN)?;
        Self::dci_write_cmd(mem, SE_CMD_STATUS_QUERY)?;

        // Read the response header (status code in upper 16 bits, length in lower 16 bits).
        let response_header = Self::dci_read_response(mem)?;
        let status_code = response_header >> 16;
        let total_len = response_header & 0xFFFF;

        if status_code != 0 {
            return Err(ArmDebugSequenceError::custom(format!(
                "EFM32xG2: SE status query failed with status code {status_code:#x}"
            ))
            .into());
        }

        // Read the remaining response words. The debug lock word is at index 3
        // for shorter responses (0x14 length) or index 7 for longer ones (0x28 length).
        let word_count = total_len.saturating_sub(4) / 4;
        let debug_lock_idx: u32 = if total_len >= 0x28 { 7 } else { 3 };
        let mut debug_lock_word = 0u32;

        for i in 0..word_count {
            let word = Self::dci_read_response(mem)?;
            if i == debug_lock_idx {
                debug_lock_word = word;
            }
        }

        if debug_lock_word & SE_STATUS_HW_DEBUG_LOCK_ACTIVE == 0 {
            return Ok(SeDebugLockStatus::Unlocked);
        }

        if debug_lock_word & SE_STATUS_DEVICE_ERASE_ENABLED == 0 {
            return Ok(SeDebugLockStatus::PermanentlyLocked);
        }

        Ok(SeDebugLockStatus::LockedEraseEnabled)
    }

    /// Issue a device erase command via DCI and wait for completion.
    fn dci_device_erase(
        interface: &mut dyn ArmDebugInterface,
        dci_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut mem = interface.memory_interface(dci_ap)?;

        Self::dci_write_cmd(&mut *mem, SE_CMD_HEADER_LEN)?;
        Self::dci_write_cmd(&mut *mem, SE_CMD_DEVICE_ERASE)?;

        // Wait for device erase to complete. The SE needs time to erase all flash.
        let ready = poll_until(DCI_ERASE_TIMEOUT, Duration::from_millis(100), || {
            let status = mem.read_word_32(DCI_STATUS)?;
            Ok(status & DCI_STATUS_RDATAVALID != 0)
        })?;
        if !ready {
            return Err(ArmError::Timeout);
        }

        // Read and verify the erase response.
        let erase_response = mem.read_word_32(DCI_RDATA)?;
        let erase_status = erase_response >> 16;

        if erase_status != 0 {
            return Err(ArmDebugSequenceError::custom(format!(
                "EFM32xG2: DCI device erase failed with status code {erase_status:#x}"
            ))
            .into());
        }

        Ok(())
    }

    fn dci_apply_lock(
        interface: &mut dyn ArmDebugInterface,
        dci_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut mem = interface.memory_interface(dci_ap)?;

        Self::dci_write_cmd(&mut *mem, SE_CMD_HEADER_LEN)?;
        Self::dci_write_cmd(&mut *mem, SE_CMD_APPLY_LOCK)?;

        // Wait for locking to complete. The SE needs time to erase all flash.
        let ready = poll_until(DCI_ERASE_TIMEOUT, Duration::from_millis(100), || {
            let status = mem.read_word_32(DCI_STATUS)?;
            Ok(status & DCI_STATUS_RDATAVALID != 0)
        })?;
        if !ready {
            return Err(ArmError::Timeout);
        }

        // Read and verify the erase response.
        let erase_response = mem.read_word_32(DCI_RDATA)?;
        let erase_status = erase_response >> 16;

        if erase_status != 0 {
            return Err(ArmDebugSequenceError::custom(format!(
                "EFM32xG2: DCI apply lock failed with status code {erase_status:#x}"
            ))
            .into());
        }

        Ok(())
    }

    /// Toggle the nRST pin to perform a hardware reset.
    fn nrst_pin_reset(interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let nreset: u32 = 0x80;
        interface.swj_pins(0, nreset, 0)?;
        thread::sleep(Duration::from_millis(100));
        interface.swj_pins(nreset, nreset, 0)?;
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }
}

impl ArmDebugSequence for EFM32xG2 {
    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // Quick check: read CSW on the default AP to see if the device is already unlocked.
        if Self::is_device_unlocked(interface, default_ap)? {
            tracing::debug!("EFM32xG2: Device is already unlocked");
            return Ok(());
        }

        // Query SE status via DCI to determine if erase-unlock is possible.
        tracing::info!("EFM32xG2: Device is locked, querying SE status via DCI");
        let dci_ap = FullyQualifiedApAddress::v1_with_dp(default_ap.dp(), 1);
        let lock_status = {
            let mut mem = interface.memory_interface(&dci_ap)?;
            Self::se_query_debug_lock_status(&mut *mem)?
        };

        match lock_status {
            SeDebugLockStatus::Unlocked => {
                tracing::info!("EFM32xG2: SE reports debug lock is not active");
                return Ok(());
            }
            SeDebugLockStatus::PermanentlyLocked => {
                return Err(ArmDebugSequenceError::custom(
                    "EFM32xG2: Device is locked and device erase is disabled (permanent lock)",
                )
                .into());
            }
            SeDebugLockStatus::LockedEraseEnabled => {}
        }

        // Perform DCI device erase (requires erase_all permission).
        tracing::debug!("EFM32xG2: Device is locked. Performing DCI device erase to unlock.");
        permissions
            .erase_all()
            .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;
        Self::dci_device_erase(interface, &dci_ap)?;
        tracing::info!("EFM32xG2: DCI device erase completed successfully, resetting target");

        // Give the SE time to complete its internal reset and clear the lock configuration.
        thread::sleep(Duration::from_secs(2));

        Self::nrst_pin_reset(interface)?;

        Err(ArmError::ReAttachRequired)
    }

    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_vector = core.read_word_32(self.flash_base_addr + 0x4)?;

        if reset_vector != 0xffff_ffff {
            tracing::info!("Breakpoint on user application reset vector");
            core.write_word_32(0xE000_2008, reset_vector | 1)?;
            core.write_word_32(0xE000_2000, 3)?;
        }

        if reset_vector == 0xffff_ffff {
            tracing::info!("Enable reset vector catch");
            let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
            demcr.set_vc_corereset(true);
            core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        }

        let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        core.write_word_32(0xE000_2008, 0x0)?;
        core.write_word_32(0xE000_2000, 0x2)?;

        let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        cortex_m_wait_for_reset(interface)?;

        let dhcsr = Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?);
        if dhcsr.s_lockup() {
            // Try to resolve lockup by halting the core again with a modified version of SiLab's
            // application note AN0062 'Programming Internal Flash Over the Serial Wire Debug
            // Interface', section 3.1 'Halting the CPU'
            // (https://www.silabs.com/documents/public/application-notes/an0062.pdf).
            //
            // Using just SYSRESETREQ did not work for mass-erased EFM32xG2/Cortex-M33 devices. But
            // using VECTRESET instead, like OpenOCD documents as its default and as it can be seen
            // from Simplicity Commander, does the trick.

            // Request halting the core for debugging.
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), value.into())?;

            // Request halt-on-reset.
            let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
            demcr.set_vc_corereset(true);
            interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

            // Trigger reset.
            let mut aircr = Aircr(0);
            aircr.vectkey();
            aircr.set_vectreset(true);
            aircr.set_vectclractive(true);
            interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

            cortex_m_wait_for_reset(interface)?;

            // We should no longer be in lokup state at this point. CoreInterface::status is going
            // to chek this soon.
        }

        Ok(())
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        if self.use_msc_erase {
            Some(Arc::new(MscEraseSequence {}))
        } else {
            None
        }
    }

    fn debug_lock_sequence(&self) -> Option<Arc<dyn DebugLockSequence>> {
        Some(Arc::new(DCIDebugLockSequence {}))
    }
}

#[derive(Debug)]
pub(crate) struct MscEraseSequence;

impl DebugEraseSequence for MscEraseSequence {
    fn erase_all(
        &self,
        interface: &mut dyn crate::architecture::arm::ArmDebugInterface,
    ) -> Result<(), ArmError> {
        let mut mem =
            interface.memory_interface(&FullyQualifiedApAddress::v1_with_default_dp(0))?;

        const CMU_BASE: u64 = 0x4000_8000;
        const CMU_CLKEN1_SET: u64 = CMU_BASE + 0x1068;
        const CMU_CLKEN1_SET_MSC: u32 = 1 << 16;

        // Enable MSC clock
        mem.write_word_32(CMU_CLKEN1_SET, CMU_CLKEN1_SET_MSC)?;

        const MSC_BASE: u64 = 0x4003_0000;
        const MSC_WRITECTRL: u64 = MSC_BASE + 0x0C;
        const MSC_WRITECTRL_WREN: u32 = 1;
        const MSC_WRITECMD: u64 = MSC_BASE + 0x10;
        const MSC_WRITECMD_ERASEMAIN0: u32 = 1 << 8;
        const MSC_STATUS: u64 = MSC_BASE + 0x1C;
        const MSC_STATUS_BUSY: u32 = 1;

        // Enable flash write/erase
        mem.write_word_32(MSC_WRITECTRL, MSC_WRITECTRL_WREN)?;

        // Initiate mass erase
        mem.write_word_32(MSC_WRITECMD, MSC_WRITECMD_ERASEMAIN0)?;

        // Poll status until erase is complete
        let start = Instant::now();
        loop {
            let status = mem.read_word_32(MSC_STATUS)?;
            if status & MSC_STATUS_BUSY == 0 {
                break;
            }

            if start.elapsed().as_millis() > 2000 {
                Err(ArmError::Timeout)?;
            }

            thread::sleep(Duration::from_millis(10));
        }

        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct DCIDebugLockSequence;

/// This sequence implements Locking and Unlocking
///
/// Please refer to Silab's application note AN1190 for details
impl DebugLockSequence for DCIDebugLockSequence {
    fn supported_lock_levels(&self) -> Vec<LockLevel> {
        vec![LockLevel {
            name: "standard".into(),
            description: "An DCI Erase Device command followed by a reset".into(),
            is_permanent: false,
        }]
    }

    fn lock(&self, interface: &mut dyn ArmDebugInterface, _level: &str) -> Result<(), ArmError> {
        let dci_ap = FullyQualifiedApAddress::v1_with_default_dp(1);
        EFM32xG2::dci_apply_lock(interface, &dci_ap)
    }
}
