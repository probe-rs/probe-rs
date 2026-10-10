//! Sequences for the nRF devices.

use crate::{
    architecture::arm::{
        ApAddress, ArmDebugInterface, ArmError, FullyQualifiedApAddress,
        ap::CSW,
        core::armv7m::Dhcsr,
        dp::DpAddress,
        memory::ArmMemoryInterface,
        sequences::{
            ArmDebugSequence, ArmDebugSequenceError, DebugEraseSequence, cortex_m_reset_system,
            cortex_m_wait_for_reset,
        },
    },
    core::MemoryMappedRegister,
    session::MissingPermissions,
};
use std::{
    fmt::Debug,
    sync::Arc,
    time::{Duration, Instant},
};

pub trait Nrf: Sync + Send + Debug {
    /// Returns the ahb_ap and ctrl_ap of every core
    fn core_aps(
        &self,
        dp_address: &DpAddress,
    ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)>;

    /// Returns true when the core is unlocked and false when it is locked.
    fn is_core_unlocked(
        &self,
        interface: &mut dyn ArmDebugInterface,
        ahb_ap_address: &FullyQualifiedApAddress,
        ctrl_ap_address: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError>;

    /// Returns true if a network core is present
    fn has_network_core(&self) -> bool;

    /// Returns the CTRL-AP used to reset `core_ap`, or `None` to use the generic Cortex-M reset.
    fn ctrl_ap_for_core(
        &self,
        _core_ap: &FullyQualifiedApAddress,
    ) -> Result<Option<FullyQualifiedApAddress>, ArmError> {
        Ok(None)
    }

    /// Reset the selected core. Implementors can override this while retaining
    /// the shared nRF pre- and post-reset orchestration.
    fn reset_core(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let core_ap = interface.fully_qualified_address();
        let Some(ctrl_ap) = self.ctrl_ap_for_core(&core_ap)? else {
            return cortex_m_reset_system(interface);
        };

        let arm_interface = interface.get_arm_debug_interface()?;
        tracing::debug!(?core_ap, ?ctrl_ap, "Asserting core reset through CTRL-AP");
        arm_interface.write_raw_ap_register(&ctrl_ap, RESET, 1)?;
        arm_interface.write_raw_ap_register(&ctrl_ap, RESET, 0)?;
        arm_interface.flush()?;

        cortex_m_wait_for_reset(interface)
    }

    /// Restore target-specific debug access after resetting the selected core.
    fn post_reset(&self, _interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        Ok(())
    }

    /// Returns true if the chip must be soft-reset after an erase-all operation (ie to unlock APPROTECT).
    ///
    /// Defaults to false. For implementors, make sure to override this method if a reset is required.
    fn requires_soft_reset_after_erase(&self) -> bool {
        false
    }
}

const RESET: u64 = 0x00;
const ERASEALL: u64 = 0x04;
const ERASEALLSTATUS: u64 = 0x08;
const ERASEALL_STATUS_POLL_LIMIT: usize = 150;
const ERASEALL_STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);

const APPLICATION_SPU_PERIPH_PERM: u64 = 0x50003800;

const APPLICATION_RESET_PERIPH_ID: u64 = 5;
const APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER: u32 = 0x50005614;
const APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER: u32 = 0x40005614;
// Nordic's current nrfx uses these hidden FICR words to gate nRF5340 Erratum 161:
// https://github.com/NordicSemiconductor/nrfx/blob/master/bsp/stable/mdk/nrf53/nrf53_erratas.h
const APPLICATION_FICR_ERRATA_VAR1_REGISTER: u64 = 0x00ff0130;
const APPLICATION_FICR_ERRATA_VAR2_REGISTER: u64 = 0x00ff0134;
const NETWORK_FORCEOFF_WORKAROUND_OFFSET: u32 = 4;
const RELEASE_FORCEOFF: u32 = 0;
const HOLD_FORCEOFF: u32 = 1;
const ERRATUM_161_RELEASE_DELAY: Duration = Duration::from_micros(5);
const ERRATUM_161_HOLD_DELAY: Duration = Duration::from_micros(1);
// This is a conservative host-side deadline, not a Nordic-specified startup maximum.
const NETWORK_CORE_ACCESS_TIMEOUT: Duration = Duration::from_millis(300);

pub(super) trait NetworkForceoffInterface {
    fn read_forceoff_word(&mut self, address: u64) -> Result<u32, ArmError>;
    fn write_forceoff_word(&mut self, address: u64, value: u32) -> Result<(), ArmError>;
    fn flush_forceoff(&mut self) -> Result<(), ArmError>;
    fn sleep_forceoff(&mut self, duration: Duration);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NetworkCoreRelease {
    AlreadyReleased,
    Released,
}

impl<T: ArmMemoryInterface + ?Sized> NetworkForceoffInterface for T {
    fn read_forceoff_word(&mut self, address: u64) -> Result<u32, ArmError> {
        self.read_word_32(address)
    }

    fn write_forceoff_word(&mut self, address: u64, value: u32) -> Result<(), ArmError> {
        self.write_word_32(address, value)
    }

    fn flush_forceoff(&mut self) -> Result<(), ArmError> {
        self.flush()
    }

    fn sleep_forceoff(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }
}

trait CtrlApRecoveryInterface {
    fn read_recovery_register(
        &mut self,
        ctrl_ap: &FullyQualifiedApAddress,
        register: u64,
    ) -> Result<u32, ArmError>;
    fn write_recovery_register(
        &mut self,
        ctrl_ap: &FullyQualifiedApAddress,
        register: u64,
        value: u32,
    ) -> Result<(), ArmError>;
    fn flush_recovery(&mut self) -> Result<(), ArmError>;
    fn sleep_recovery(&mut self, duration: Duration);
    fn restore_network_after_erase(&mut self, access: &NetworkEraseAccess) -> Result<(), ArmError>;
}

#[derive(Clone, Debug)]
struct NetworkEraseAccess {
    application_ahb_ap: FullyQualifiedApAddress,
    network_ahb_ap: FullyQualifiedApAddress,
}

#[derive(Debug)]
enum PostEraseNetworkAccess {
    NotRequired,
    Restore(NetworkEraseAccess),
    MissingMapping,
}

impl<T: ArmDebugInterface + ?Sized> CtrlApRecoveryInterface for T {
    fn read_recovery_register(
        &mut self,
        ctrl_ap: &FullyQualifiedApAddress,
        register: u64,
    ) -> Result<u32, ArmError> {
        self.read_raw_ap_register(ctrl_ap, register)
    }

    fn write_recovery_register(
        &mut self,
        ctrl_ap: &FullyQualifiedApAddress,
        register: u64,
        value: u32,
    ) -> Result<(), ArmError> {
        self.write_raw_ap_register(ctrl_ap, register, value)
    }

    fn flush_recovery(&mut self) -> Result<(), ArmError> {
        self.flush()
    }

    fn sleep_recovery(&mut self, duration: Duration) {
        std::thread::sleep(duration);
    }

    fn restore_network_after_erase(&mut self, access: &NetworkEraseAccess) -> Result<(), ArmError> {
        {
            let mut application_interface = self.memory_interface(&access.application_ahb_ap)?;
            set_network_core_running(&mut *application_interface)?;
        }

        let started = Instant::now();
        loop {
            let csw: CSW = self
                .read_raw_ap_register(&access.network_ahb_ap, 0x00)?
                .try_into()?;
            if csw.DeviceEn() {
                return Ok(());
            }
            if started.elapsed() >= NETWORK_CORE_ACCESS_TIMEOUT {
                return Err(ArmDebugSequenceError::custom(
                    "Network core did not become accessible after chip erase",
                )
                .into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

fn preserve_primary_error(
    primary: Result<(), ArmError>,
    cleanup: Result<(), ArmError>,
    cleanup_context: &'static str,
) -> Result<(), ArmError> {
    match (primary, cleanup) {
        (Err(primary), Err(cleanup)) => {
            tracing::warn!(?cleanup, "{cleanup_context}");
            Err(primary)
        }
        (Err(primary), Ok(())) => Err(primary),
        (Ok(()), cleanup) => cleanup,
    }
}

fn perform_reset_after_erase(
    interface: &mut (impl CtrlApRecoveryInterface + ?Sized),
    ctrl_ap: &FullyQualifiedApAddress,
) -> Result<(), ArmError> {
    let reset_result = (|| {
        interface.write_recovery_register(ctrl_ap, RESET, 1)?;
        interface.flush_recovery()?;
        interface.write_recovery_register(ctrl_ap, RESET, 0)?;
        interface.flush_recovery()
    })();

    if reset_result.is_ok() {
        return Ok(());
    }

    // A failed or batched reset transaction must not leave the core held in
    // reset. Preserve the original transport failure while reporting cleanup.
    let cleanup_write = interface.write_recovery_register(ctrl_ap, RESET, 0);
    let cleanup_flush = interface.flush_recovery();
    let cleanup_result = preserve_primary_error(
        cleanup_write,
        cleanup_flush,
        "CTRL-AP reset cleanup flush also failed",
    );
    preserve_primary_error(
        reset_result,
        cleanup_result,
        "CTRL-AP reset cleanup also failed",
    )
}

fn perform_erase_all(
    interface: &mut (impl CtrlApRecoveryInterface + ?Sized),
    ctrl_ap: &FullyQualifiedApAddress,
    reset_after_erase: bool,
) -> Result<(), ArmError> {
    interface.write_recovery_register(ctrl_ap, ERASEALL, 1)?;
    interface.flush_recovery()?;

    for attempt in 0..=ERASEALL_STATUS_POLL_LIMIT {
        if interface.read_recovery_register(ctrl_ap, ERASEALLSTATUS)? == 0 {
            break;
        }

        if attempt == ERASEALL_STATUS_POLL_LIMIT {
            return Err(ArmError::Timeout);
        }
        interface.sleep_recovery(ERASEALL_STATUS_POLL_INTERVAL);
    }

    if reset_after_erase {
        tracing::debug!(?ctrl_ap, "Resetting core after erase operation");
        let reset_result = perform_reset_after_erase(interface, ctrl_ap);
        // Let the reset exit settle before callers attempt memory access,
        // including after a reset transport error and best-effort deassertion.
        interface.sleep_recovery(Duration::from_millis(20));
        reset_result?;
    }

    Ok(())
}

/// Performs an erase all operation on the core.
/// The `ap_address` must be of the ctrl ap of the core.
fn erase_all(
    arm_interface: &mut (impl CtrlApRecoveryInterface + ?Sized),
    ap_address: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
    reset_after_erase: bool,
) -> Result<(), ArmError> {
    permissions
        .erase_all()
        .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

    perform_erase_all(arm_interface, ap_address, reset_after_erase)
}

/// Performs an erase all procedure to unlock the core.
/// The `ap_address` must be of the ctrl ap of the core.
fn unlock_core(
    arm_interface: &mut dyn ArmDebugInterface,
    ap_address: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
    reset_after_erase: bool,
) -> Result<(), ArmError> {
    erase_all(arm_interface, ap_address, permissions, reset_after_erase)
}

fn erratum_161_present(
    interface: &mut (impl NetworkForceoffInterface + ?Sized),
) -> Result<bool, ArmError> {
    // This runs through application AP0, which is also used above to read the
    // secure SPU attribution register. RESET's attribution does not change the
    // application FICR address used by the debugger.
    let var1 = interface.read_forceoff_word(APPLICATION_FICR_ERRATA_VAR1_REGISTER)?;
    let var2 = interface.read_forceoff_word(APPLICATION_FICR_ERRATA_VAR2_REGISTER)?;

    // nrf53_errata_161() marks variants 0x02 through 0x04 unaffected and
    // variant 0x05 plus later/default encodings affected.
    Ok(var1 == 0x07 && !matches!(var2, 0x02..=0x04))
}

fn clear_forceoff_workaround(
    interface: &mut (impl NetworkForceoffInterface + ?Sized),
    workaround_addr: u64,
) -> Result<(), ArmError> {
    interface.write_forceoff_word(workaround_addr, 0)?;
    interface.flush_forceoff()
}

fn apply_erratum_161_release(
    interface: &mut (impl NetworkForceoffInterface + ?Sized),
    forceoff_addr: u64,
) -> Result<(), ArmError> {
    // Follow Nordic's release/hold/release sequence and explicit minimum delays:
    // https://github.com/NordicSemiconductor/nrfx/blob/master/hal/nrf_reset.h
    let workaround_addr = forceoff_addr + u64::from(NETWORK_FORCEOFF_WORKAROUND_OFFSET);
    let sequence_result = (|| {
        interface.write_forceoff_word(workaround_addr, 1)?;
        interface.flush_forceoff()?;

        interface.write_forceoff_word(forceoff_addr, RELEASE_FORCEOFF)?;
        interface.flush_forceoff()?;
        interface.sleep_forceoff(ERRATUM_161_RELEASE_DELAY);

        interface.write_forceoff_word(forceoff_addr, HOLD_FORCEOFF)?;
        interface.flush_forceoff()?;
        interface.sleep_forceoff(ERRATUM_161_HOLD_DELAY);

        interface.write_forceoff_word(forceoff_addr, RELEASE_FORCEOFF)?;
        interface.flush_forceoff()
    })();

    if sequence_result.is_err() {
        let _ = interface.write_forceoff_word(forceoff_addr, HOLD_FORCEOFF);
        let _ = interface.flush_forceoff();
    }

    let cleanup_result = clear_forceoff_workaround(interface, workaround_addr);
    sequence_result.and(cleanup_result)
}

/// Release network FORCEOFF without resetting an already-running network core.
pub(super) fn set_network_core_running(
    interface: &mut (impl NetworkForceoffInterface + ?Sized),
) -> Result<NetworkCoreRelease, ArmError> {
    // Determine if the RESET peripheral is mapped to secure or non-secure address space.
    let periph_config_address = APPLICATION_SPU_PERIPH_PERM + 0x4 * APPLICATION_RESET_PERIPH_ID;
    let periph_config = interface.read_forceoff_word(periph_config_address)?;
    let is_secure = (periph_config >> 4) & 1 == 1;

    let forceoff_addr = if is_secure {
        tracing::debug!("RESET peripheral is mapped to secure address space");
        APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER
    } else {
        tracing::debug!("RESET peripheral is mapped to non-secure address space");
        APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER
    };

    if interface.read_forceoff_word(forceoff_addr as u64)? == RELEASE_FORCEOFF {
        tracing::debug!("Network core is already released from FORCEOFF");
        return Ok(NetworkCoreRelease::AlreadyReleased);
    }

    if erratum_161_present(interface)? {
        tracing::debug!("Applying nRF5340 Revision 1 Erratum 161 workaround");
        apply_erratum_161_release(interface, forceoff_addr as u64)?;
        return Ok(NetworkCoreRelease::Released);
    }

    interface.write_forceoff_word(forceoff_addr as u64, RELEASE_FORCEOFF)?;
    interface.flush_forceoff()?;
    Ok(NetworkCoreRelease::Released)
}

pub(super) fn reset_affects_network<T: Nrf + ?Sized>(
    sequence: &T,
    core_ap: &FullyQualifiedApAddress,
) -> bool {
    sequence.has_network_core() && core_ap.ap() == &ApAddress::V1(0)
}

fn wait_for_network_core_halted(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let started = Instant::now();

    loop {
        if Dhcsr(interface.read_word_32(Dhcsr::get_mmio_address())?).s_halt() {
            return Ok(());
        }

        if started.elapsed() >= NETWORK_CORE_ACCESS_TIMEOUT {
            return Err(ArmError::Timeout);
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}

pub(super) fn halt_network_core(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.set_c_halt(true);
    dhcsr.enable_write();
    interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
    interface.flush()?;
    wait_for_network_core_halted(interface)
}

pub(super) fn wait_for_core_accessible<T: Nrf + ?Sized>(
    sequence: &T,
    interface: &mut dyn ArmDebugInterface,
    ahb_ap_address: &FullyQualifiedApAddress,
    ctrl_ap_address: &FullyQualifiedApAddress,
) -> Result<bool, ArmError> {
    let started = Instant::now();

    loop {
        if sequence.is_core_unlocked(interface, ahb_ap_address, ctrl_ap_address)? {
            return Ok(true);
        }

        if started.elapsed() >= NETWORK_CORE_ACCESS_TIMEOUT {
            tracing::debug!(
                "Network core remained inaccessible for {} ms",
                NETWORK_CORE_ACCESS_TIMEOUT.as_millis()
            );
            return Ok(false);
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}

fn core_is_accessible<T: Nrf + ?Sized>(
    sequence: &T,
    interface: &mut dyn ArmDebugInterface,
    ahb_ap_address: &FullyQualifiedApAddress,
    ctrl_ap_address: &FullyQualifiedApAddress,
    wait_for_network_core: bool,
) -> Result<bool, ArmError> {
    if wait_for_network_core {
        wait_for_core_accessible(sequence, interface, ahb_ap_address, ctrl_ap_address)
    } else {
        sequence.is_core_unlocked(interface, ahb_ap_address, ctrl_ap_address)
    }
}

trait CoreUnlockOperations {
    fn prepare_network_core(
        &mut self,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<NetworkCoreRelease, ArmError>;
    fn is_core_accessible<T: Nrf + ?Sized>(
        &mut self,
        sequence: &T,
        ahb_ap_address: &FullyQualifiedApAddress,
        ctrl_ap_address: &FullyQualifiedApAddress,
        wait_for_network_core: bool,
    ) -> Result<bool, ArmError>;
    fn recover_core(
        &mut self,
        ctrl_ap_address: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
        reset_after_erase: bool,
    ) -> Result<(), ArmError>;
    fn halt_core(&mut self, ahb_ap_address: &FullyQualifiedApAddress) -> Result<(), ArmError>;
}

struct ArmCoreUnlockOperations<'a> {
    interface: &'a mut dyn ArmDebugInterface,
}

impl CoreUnlockOperations for ArmCoreUnlockOperations<'_> {
    fn prepare_network_core(
        &mut self,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<NetworkCoreRelease, ArmError> {
        let mut memory_interface = self.interface.memory_interface(default_ap)?;
        set_network_core_running(&mut *memory_interface)
    }

    fn is_core_accessible<T: Nrf + ?Sized>(
        &mut self,
        sequence: &T,
        ahb_ap_address: &FullyQualifiedApAddress,
        ctrl_ap_address: &FullyQualifiedApAddress,
        wait_for_network_core: bool,
    ) -> Result<bool, ArmError> {
        core_is_accessible(
            sequence,
            self.interface,
            ahb_ap_address,
            ctrl_ap_address,
            wait_for_network_core,
        )
    }

    fn recover_core(
        &mut self,
        ctrl_ap_address: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
        reset_after_erase: bool,
    ) -> Result<(), ArmError> {
        unlock_core(
            self.interface,
            ctrl_ap_address,
            permissions,
            reset_after_erase,
        )
    }

    fn halt_core(&mut self, ahb_ap_address: &FullyQualifiedApAddress) -> Result<(), ArmError> {
        let mut network_interface = self.interface.memory_interface(ahb_ap_address)?;
        halt_network_core(&mut *network_interface)
    }
}

fn initialize_core_access<T: Nrf + ?Sized>(
    sequence: &T,
    operations: &mut impl CoreUnlockOperations,
    default_ap: &FullyQualifiedApAddress,
    permissions: &crate::Permissions,
) -> Result<(), ArmError> {
    let aps = sequence.core_aps(&default_ap.dp());

    if aps.is_empty() {
        return Err(ArmDebugSequenceError::custom(
            "Target does not declare a supported Nordic core access port",
        )
        .into());
    }

    for (core_index, (core_ahb_ap_address, core_ctrl_ap_address)) in aps.iter().enumerate() {
        let is_network_core =
            sequence.has_network_core() && core_ahb_ap_address.ap() == &ApAddress::V1(1);
        let mut quiesce_network_core = false;
        if is_network_core {
            tracing::debug!("Releasing network core before accessing its AHB-AP");
            quiesce_network_core =
                operations.prepare_network_core(default_ap)? == NetworkCoreRelease::Released;
        }

        tracing::info!("Checking if core {} is accessible", core_index);
        let is_accessible = operations.is_core_accessible(
            sequence,
            core_ahb_ap_address,
            core_ctrl_ap_address,
            is_network_core,
        )?;

        if is_accessible {
            tracing::info!("Core {} is already accessible", core_index);
        } else {
            tracing::warn!(
                "Core {} is not accessible. Erase procedure will be started to recover access.",
                core_index
            );
            if is_network_core {
                tracing::warn!(
                    "CTRL-AP3 recovery erases application and network flash, UICR, RAM, and peripheral state"
                );
            }

            let recovery_result = operations.recover_core(
                core_ctrl_ap_address,
                permissions,
                sequence.requires_soft_reset_after_erase(),
            );
            quiesce_network_core |= is_network_core;

            if is_network_core {
                // AP3 reset makes the post-ERASEALL protection state take
                // effect. Re-release FORCEOFF even when recovery reports an
                // error, while preserving that primary recovery error.
                let prepare_result = operations.prepare_network_core(default_ap).map(|_| ());
                preserve_primary_error(
                    recovery_result,
                    prepare_result,
                    "Network preparation after CTRL-AP3 recovery also failed",
                )?;
            } else {
                recovery_result?;
            }

            if !operations.is_core_accessible(
                sequence,
                core_ahb_ap_address,
                core_ctrl_ap_address,
                is_network_core,
            )? {
                // Do not silently issue a second destructive ERASEALL.
                return Err(ArmDebugSequenceError::custom(format!(
                    "Could not access core {core_index} after erase operation"
                ))
                .into());
            }
        }

        if quiesce_network_core {
            tracing::debug!("Halting newly released network core");
            operations.halt_core(core_ahb_ap_address)?;
        }
    }

    Ok(())
}

impl<T: Nrf> ArmDebugSequence for T {
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: crate::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let reset_result = self.reset_core(interface, core_type, debug_base);
        if let Err(post_reset_error) = self.post_reset(interface) {
            if reset_result.is_ok() {
                return Err(post_reset_error);
            }
            tracing::warn!(
                ?post_reset_error,
                "Network post-reset preparation also failed"
            );
        }
        reset_result
    }

    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        // TODO: Eraseprotect is not considered. If enabled, the debugger must set up the same keys as the firmware does
        // TODO: Approtect and Secure Approtect are not considered. If enabled, the debugger must set up the same keys as the firmware does
        // These keys should be queried from the user if required and once that mechanism is implemented
        initialize_core_access(
            self,
            &mut ArmCoreUnlockOperations { interface },
            default_ap,
            permissions,
        )
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        let core_aps = self.core_aps(&DpAddress::Default);
        let post_erase_network_access = if self.has_network_core() {
            let application_ahb_ap = core_aps
                .iter()
                .find(|(ahb_ap, _)| ahb_ap.ap() == &ApAddress::V1(0))
                .map(|(ahb_ap, _)| ahb_ap.clone());
            let network_ahb_ap = core_aps
                .iter()
                .find(|(ahb_ap, _)| ahb_ap.ap() == &ApAddress::V1(1))
                .map(|(ahb_ap, _)| ahb_ap.clone());

            match (application_ahb_ap, network_ahb_ap) {
                (Some(application_ahb_ap), Some(network_ahb_ap)) => {
                    PostEraseNetworkAccess::Restore(NetworkEraseAccess {
                        application_ahb_ap,
                        network_ahb_ap,
                    })
                }
                _ => PostEraseNetworkAccess::MissingMapping,
            }
        } else {
            PostEraseNetworkAccess::NotRequired
        };

        Some(Arc::new(NrfDebugEraseSequence {
            ctrl_aps: core_aps
                .into_iter()
                .map(|(_ahb_ap, ctrl_ap)| ctrl_ap)
                .collect(),
            reset_after_erase: self.requires_soft_reset_after_erase(),
            post_erase_network_access,
        }))
    }
}

/// Chip erase via the ERASEALL register of each core's CTRL-AP.
///
/// Unlike the flash algorithms from the vendor's CMSIS-Pack, this erases *all* nonvolatile
/// memory, including the UICR, which some of the pack algorithms cannot erase at all
/// (the UICR of nRF53 and nRF91 chips can only be erased by an ERASEALL operation).
///
/// TODO: Eraseprotect is not considered, same as in `debug_device_unlock` above. If enabled,
/// the hardware ignores the ERASEALL request and this sequence reports success anyway.
#[derive(Debug)]
struct NrfDebugEraseSequence {
    ctrl_aps: Vec<FullyQualifiedApAddress>,
    reset_after_erase: bool,
    post_erase_network_access: PostEraseNetworkAccess,
}

fn perform_debug_erase_all(
    interface: &mut (impl CtrlApRecoveryInterface + ?Sized),
    ctrl_aps: &[FullyQualifiedApAddress],
    permissions: &crate::Permissions,
    reset_after_erase: bool,
    post_erase_network_access: &PostEraseNetworkAccess,
) -> Result<(), ArmError> {
    let network_access = match post_erase_network_access {
        PostEraseNetworkAccess::NotRequired => None,
        PostEraseNetworkAccess::Restore(access) => Some(access),
        PostEraseNetworkAccess::MissingMapping => {
            return Err(ArmDebugSequenceError::custom(
                "Dual-core nRF5340 target has incomplete application/network AP mappings",
            )
            .into());
        }
    };

    let erase_result = ctrl_aps
        .iter()
        .try_for_each(|ctrl_ap| erase_all(interface, ctrl_ap, permissions, reset_after_erase));

    if let Some(access) = network_access {
        let restore_result = interface.restore_network_after_erase(access);
        preserve_primary_error(
            erase_result,
            restore_result,
            "Network restoration after CTRL-AP erase also failed",
        )
    } else {
        erase_result
    }
}

impl DebugEraseSequence for NrfDebugEraseSequence {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        // Chip erase is only requested by an explicit user action (`--chip-erase` or an erase
        // command), which stands in for the `erase_all` permission here.
        let permissions = crate::Permissions::new().allow_erase_all();
        perform_debug_erase_all(
            interface,
            &self.ctrl_aps,
            &permissions,
            self.reset_after_erase,
            &self.post_erase_network_access,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestNrf {
        core_aps: Vec<(u8, u8)>,
    }

    impl Nrf for TestNrf {
        fn core_aps(
            &self,
            dp_address: &DpAddress,
        ) -> Vec<(FullyQualifiedApAddress, FullyQualifiedApAddress)> {
            self.core_aps
                .iter()
                .map(|&(ahb_ap, ctrl_ap)| {
                    (
                        FullyQualifiedApAddress::v1_with_dp(*dp_address, ahb_ap),
                        FullyQualifiedApAddress::v1_with_dp(*dp_address, ctrl_ap),
                    )
                })
                .collect()
        }

        fn is_core_unlocked(
            &self,
            _interface: &mut dyn ArmDebugInterface,
            _ahb_ap_address: &FullyQualifiedApAddress,
            _ctrl_ap_address: &FullyQualifiedApAddress,
        ) -> Result<bool, ArmError> {
            unreachable!("mock operations provide accessibility results")
        }

        fn has_network_core(&self) -> bool {
            self.core_aps.iter().any(|&(ahb_ap, _)| ahb_ap == 1)
        }
    }

    #[derive(Debug, PartialEq)]
    enum UnlockOperation {
        PrepareNetwork,
        Check(ApAddress),
        Recover(ApAddress),
        Halt(ApAddress),
    }

    struct MockCoreUnlockOperations {
        network_release: NetworkCoreRelease,
        network_prepare_calls: usize,
        network_prepare_error: Option<(usize, ArmError)>,
        accessibility: std::collections::VecDeque<bool>,
        recovery_error: Option<ArmError>,
        operations: Vec<UnlockOperation>,
    }

    impl CoreUnlockOperations for MockCoreUnlockOperations {
        fn prepare_network_core(
            &mut self,
            _default_ap: &FullyQualifiedApAddress,
        ) -> Result<NetworkCoreRelease, ArmError> {
            self.operations.push(UnlockOperation::PrepareNetwork);
            self.network_prepare_calls += 1;
            if self
                .network_prepare_error
                .as_ref()
                .is_some_and(|(call, _)| *call == self.network_prepare_calls)
            {
                return Err(self.network_prepare_error.take().unwrap().1);
            }
            Ok(self.network_release)
        }

        fn is_core_accessible<T: Nrf + ?Sized>(
            &mut self,
            _sequence: &T,
            ahb_ap_address: &FullyQualifiedApAddress,
            _ctrl_ap_address: &FullyQualifiedApAddress,
            _wait_for_network_core: bool,
        ) -> Result<bool, ArmError> {
            self.operations
                .push(UnlockOperation::Check(ahb_ap_address.ap().clone()));
            Ok(self.accessibility.pop_front().unwrap_or(true))
        }

        fn recover_core(
            &mut self,
            ctrl_ap_address: &FullyQualifiedApAddress,
            _permissions: &crate::Permissions,
            _reset_after_erase: bool,
        ) -> Result<(), ArmError> {
            self.operations
                .push(UnlockOperation::Recover(ctrl_ap_address.ap().clone()));
            self.recovery_error.take().map_or(Ok(()), Err)
        }

        fn halt_core(&mut self, ahb_ap_address: &FullyQualifiedApAddress) -> Result<(), ArmError> {
            self.operations
                .push(UnlockOperation::Halt(ahb_ap_address.ap().clone()));
            Ok(())
        }
    }

    #[test]
    fn dual_core_unlock_prepares_network_before_ap1_and_only_halts_new_release() {
        let sequence = TestNrf {
            core_aps: vec![(0, 2), (1, 3)],
        };

        for (network_release, expected) in [
            (
                NetworkCoreRelease::Released,
                vec![
                    UnlockOperation::Check(ApAddress::V1(0)),
                    UnlockOperation::PrepareNetwork,
                    UnlockOperation::Check(ApAddress::V1(1)),
                    UnlockOperation::Halt(ApAddress::V1(1)),
                ],
            ),
            (
                NetworkCoreRelease::AlreadyReleased,
                vec![
                    UnlockOperation::Check(ApAddress::V1(0)),
                    UnlockOperation::PrepareNetwork,
                    UnlockOperation::Check(ApAddress::V1(1)),
                ],
            ),
        ] {
            let mut operations = MockCoreUnlockOperations {
                network_release,
                network_prepare_calls: 0,
                network_prepare_error: None,
                accessibility: [true, true].into(),
                recovery_error: None,
                operations: Vec::new(),
            };

            initialize_core_access(
                &sequence,
                &mut operations,
                &FullyQualifiedApAddress::v1_with_default_dp(0),
                &crate::Permissions::new(),
            )
            .unwrap();

            assert_eq!(operations.operations, expected);
        }
    }

    #[test]
    fn application_only_unlock_never_prepares_network_core() {
        let sequence = TestNrf {
            core_aps: vec![(0, 2)],
        };
        let mut operations = MockCoreUnlockOperations {
            network_release: NetworkCoreRelease::Released,
            network_prepare_calls: 0,
            network_prepare_error: None,
            accessibility: [true].into(),
            recovery_error: None,
            operations: Vec::new(),
        };

        initialize_core_access(
            &sequence,
            &mut operations,
            &FullyQualifiedApAddress::v1_with_default_dp(0),
            &crate::Permissions::new(),
        )
        .unwrap();

        assert_eq!(
            operations.operations,
            [UnlockOperation::Check(ApAddress::V1(0))]
        );
    }

    #[test]
    fn network_recovery_is_single_attempt_then_release_recheck_and_halt() {
        let sequence = TestNrf {
            core_aps: vec![(0, 2), (1, 3)],
        };
        let mut operations = MockCoreUnlockOperations {
            network_release: NetworkCoreRelease::Released,
            network_prepare_calls: 0,
            network_prepare_error: None,
            accessibility: [true, false, true].into(),
            recovery_error: None,
            operations: Vec::new(),
        };

        initialize_core_access(
            &sequence,
            &mut operations,
            &FullyQualifiedApAddress::v1_with_default_dp(0),
            &crate::Permissions::new(),
        )
        .unwrap();

        assert_eq!(
            operations.operations,
            [
                UnlockOperation::Check(ApAddress::V1(0)),
                UnlockOperation::PrepareNetwork,
                UnlockOperation::Check(ApAddress::V1(1)),
                UnlockOperation::Recover(ApAddress::V1(3)),
                UnlockOperation::PrepareNetwork,
                UnlockOperation::Check(ApAddress::V1(1)),
                UnlockOperation::Halt(ApAddress::V1(1)),
            ]
        );
    }

    #[test]
    fn failed_network_recovery_is_not_retried() {
        let sequence = TestNrf {
            core_aps: vec![(0, 2), (1, 3)],
        };
        let mut operations = MockCoreUnlockOperations {
            network_release: NetworkCoreRelease::Released,
            network_prepare_calls: 0,
            network_prepare_error: None,
            accessibility: [true, false, false].into(),
            recovery_error: None,
            operations: Vec::new(),
        };

        assert!(
            initialize_core_access(
                &sequence,
                &mut operations,
                &FullyQualifiedApAddress::v1_with_default_dp(0),
                &crate::Permissions::new(),
            )
            .is_err()
        );

        assert_eq!(
            operations.operations,
            [
                UnlockOperation::Check(ApAddress::V1(0)),
                UnlockOperation::PrepareNetwork,
                UnlockOperation::Check(ApAddress::V1(1)),
                UnlockOperation::Recover(ApAddress::V1(3)),
                UnlockOperation::PrepareNetwork,
                UnlockOperation::Check(ApAddress::V1(1)),
            ]
        );
    }

    #[test]
    fn failed_network_recovery_still_releases_forceoff() {
        let sequence = TestNrf {
            core_aps: vec![(0, 2), (1, 3)],
        };
        let mut operations = MockCoreUnlockOperations {
            network_release: NetworkCoreRelease::Released,
            network_prepare_calls: 0,
            network_prepare_error: Some((2, ArmError::OutOfBounds)),
            accessibility: [true, false].into(),
            recovery_error: Some(ArmError::Timeout),
            operations: Vec::new(),
        };

        assert!(matches!(
            initialize_core_access(
                &sequence,
                &mut operations,
                &FullyQualifiedApAddress::v1_with_default_dp(0),
                &crate::Permissions::new(),
            ),
            Err(ArmError::Timeout)
        ));

        assert_eq!(
            operations.operations,
            [
                UnlockOperation::Check(ApAddress::V1(0)),
                UnlockOperation::PrepareNetwork,
                UnlockOperation::Check(ApAddress::V1(1)),
                UnlockOperation::Recover(ApAddress::V1(3)),
                UnlockOperation::PrepareNetwork,
            ]
        );
    }

    #[derive(Debug, PartialEq)]
    enum RecoveryOperation {
        Read(ApAddress, u64),
        Write(ApAddress, u64, u32),
        Flush,
        Sleep(Duration),
        RestoreNetwork(ApAddress, ApAddress),
    }

    struct MockCtrlApRecoveryInterface {
        statuses: std::collections::VecDeque<u32>,
        fail_at_operations: std::collections::BTreeSet<usize>,
        network_restore_error: Option<ArmError>,
        operations: Vec<RecoveryOperation>,
    }

    impl MockCtrlApRecoveryInterface {
        fn new(statuses: impl IntoIterator<Item = u32>) -> Self {
            Self {
                statuses: statuses.into_iter().collect(),
                fail_at_operations: Default::default(),
                network_restore_error: None,
                operations: Vec::new(),
            }
        }

        fn should_fail(&self) -> bool {
            self.fail_at_operations.contains(&self.operations.len())
        }
    }

    impl CtrlApRecoveryInterface for MockCtrlApRecoveryInterface {
        fn read_recovery_register(
            &mut self,
            ctrl_ap: &FullyQualifiedApAddress,
            register: u64,
        ) -> Result<u32, ArmError> {
            self.operations
                .push(RecoveryOperation::Read(ctrl_ap.ap().clone(), register));
            if self.should_fail() {
                return Err(ArmError::Timeout);
            }
            Ok(self.statuses.pop_front().unwrap_or(0))
        }

        fn write_recovery_register(
            &mut self,
            ctrl_ap: &FullyQualifiedApAddress,
            register: u64,
            value: u32,
        ) -> Result<(), ArmError> {
            self.operations.push(RecoveryOperation::Write(
                ctrl_ap.ap().clone(),
                register,
                value,
            ));
            if self.should_fail() {
                return Err(ArmError::Timeout);
            }
            Ok(())
        }

        fn flush_recovery(&mut self) -> Result<(), ArmError> {
            self.operations.push(RecoveryOperation::Flush);
            if self.should_fail() {
                return Err(ArmError::Timeout);
            }
            Ok(())
        }

        fn sleep_recovery(&mut self, duration: Duration) {
            self.operations.push(RecoveryOperation::Sleep(duration));
        }

        fn restore_network_after_erase(
            &mut self,
            access: &NetworkEraseAccess,
        ) -> Result<(), ArmError> {
            self.operations.push(RecoveryOperation::RestoreNetwork(
                access.application_ahb_ap.ap().clone(),
                access.network_ahb_ap.ap().clone(),
            ));
            if self.should_fail() {
                return Err(ArmError::Timeout);
            }
            self.network_restore_error.take().map_or(Ok(()), Err)
        }
    }

    #[test]
    fn erase_all_polls_and_resets_with_flushed_edges() {
        let ctrl_ap = FullyQualifiedApAddress::v1_with_default_dp(3);
        let mut interface = MockCtrlApRecoveryInterface::new([1, 0]);

        perform_erase_all(&mut interface, &ctrl_ap, true).unwrap();

        assert_eq!(
            interface.operations,
            [
                RecoveryOperation::Write(ApAddress::V1(3), ERASEALL, 1),
                RecoveryOperation::Flush,
                RecoveryOperation::Read(ApAddress::V1(3), ERASEALLSTATUS),
                RecoveryOperation::Sleep(ERASEALL_STATUS_POLL_INTERVAL),
                RecoveryOperation::Read(ApAddress::V1(3), ERASEALLSTATUS),
                RecoveryOperation::Write(ApAddress::V1(3), RESET, 1),
                RecoveryOperation::Flush,
                RecoveryOperation::Write(ApAddress::V1(3), RESET, 0),
                RecoveryOperation::Flush,
                RecoveryOperation::Sleep(Duration::from_millis(20)),
            ]
        );
    }

    #[test]
    fn erase_all_without_reset_stops_after_ready() {
        let ctrl_ap = FullyQualifiedApAddress::v1_with_default_dp(4);
        let mut interface = MockCtrlApRecoveryInterface::new([0]);

        perform_erase_all(&mut interface, &ctrl_ap, false).unwrap();

        assert_eq!(
            interface.operations,
            [
                RecoveryOperation::Write(ApAddress::V1(4), ERASEALL, 1),
                RecoveryOperation::Flush,
                RecoveryOperation::Read(ApAddress::V1(4), ERASEALLSTATUS),
            ]
        );
    }

    #[test]
    fn erase_all_timeout_is_bounded_and_does_not_reset() {
        let ctrl_ap = FullyQualifiedApAddress::v1_with_default_dp(3);
        let mut interface = MockCtrlApRecoveryInterface::new(std::iter::repeat_n(
            1,
            ERASEALL_STATUS_POLL_LIMIT + 1,
        ));

        assert!(matches!(
            perform_erase_all(&mut interface, &ctrl_ap, true),
            Err(ArmError::Timeout)
        ));

        assert_eq!(
            interface
                .operations
                .iter()
                .filter(|operation| matches!(operation, RecoveryOperation::Read(_, ERASEALLSTATUS)))
                .count(),
            ERASEALL_STATUS_POLL_LIMIT + 1
        );
        assert!(
            !interface
                .operations
                .iter()
                .any(|operation| matches!(operation, RecoveryOperation::Write(_, RESET, _)))
        );
        assert!(matches!(
            interface.operations.last(),
            Some(RecoveryOperation::Read(_, ERASEALLSTATUS))
        ));
    }

    #[test]
    fn reset_failures_retry_deassertion_and_flush() {
        let ctrl_ap = FullyQualifiedApAddress::v1_with_default_dp(3);

        for failed_operation in 4..=7 {
            let mut interface = MockCtrlApRecoveryInterface::new([0]);
            interface.fail_at_operations.insert(failed_operation);

            assert!(perform_erase_all(&mut interface, &ctrl_ap, true).is_err());
            assert_eq!(
                &interface.operations[interface.operations.len() - 3..],
                [
                    RecoveryOperation::Write(ApAddress::V1(3), RESET, 0),
                    RecoveryOperation::Flush,
                    RecoveryOperation::Sleep(Duration::from_millis(20)),
                ]
            );
        }
    }

    #[test]
    fn debug_erase_restores_network_after_all_core_erases() {
        let mut interface = MockCtrlApRecoveryInterface::new([0, 0]);
        let ctrl_aps = [
            FullyQualifiedApAddress::v1_with_default_dp(2),
            FullyQualifiedApAddress::v1_with_default_dp(3),
        ];
        let network_access = PostEraseNetworkAccess::Restore(NetworkEraseAccess {
            application_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(0),
            network_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(1),
        });

        perform_debug_erase_all(
            &mut interface,
            &ctrl_aps,
            &crate::Permissions::new().allow_erase_all(),
            true,
            &network_access,
        )
        .unwrap();

        let erased_aps = interface
            .operations
            .iter()
            .filter_map(|operation| match operation {
                RecoveryOperation::Write(ap, ERASEALL, 1) => Some(ap.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(erased_aps, [ApAddress::V1(2), ApAddress::V1(3)]);
        assert_eq!(
            interface.operations.last(),
            Some(&RecoveryOperation::RestoreNetwork(
                ApAddress::V1(0),
                ApAddress::V1(1),
            ))
        );
    }

    #[test]
    fn debug_erase_restores_network_after_second_core_failure() {
        let ctrl_aps = [
            FullyQualifiedApAddress::v1_with_default_dp(2),
            FullyQualifiedApAddress::v1_with_default_dp(3),
        ];
        let network_access = PostEraseNetworkAccess::Restore(NetworkEraseAccess {
            application_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(0),
            network_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(1),
        });

        for failed_operation in 9..=15 {
            let mut interface = MockCtrlApRecoveryInterface::new([0, 0]);
            interface.fail_at_operations.insert(failed_operation);
            interface.network_restore_error = Some(ArmError::OutOfBounds);

            let result = perform_debug_erase_all(
                &mut interface,
                &ctrl_aps,
                &crate::Permissions::new().allow_erase_all(),
                true,
                &network_access,
            );
            assert!(
                matches!(result, Err(ArmError::Timeout)),
                "operation {failed_operation} returned {result:?}"
            );
            assert!(matches!(
                interface.operations.last(),
                Some(RecoveryOperation::RestoreNetwork(
                    ApAddress::V1(0),
                    ApAddress::V1(1)
                ))
            ));
            assert_eq!(
                interface
                    .operations
                    .iter()
                    .filter_map(|operation| match operation {
                        RecoveryOperation::Write(ap, ERASEALL, 1) => Some(ap.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>(),
                [ApAddress::V1(2), ApAddress::V1(3)]
            );
        }
    }

    #[test]
    fn debug_erase_restores_network_without_starting_ap3_after_ap2_failure() {
        let mut interface = MockCtrlApRecoveryInterface::new([0, 0]);
        interface.fail_at_operations.insert(7);
        interface.network_restore_error = Some(ArmError::OutOfBounds);
        let ctrl_aps = [
            FullyQualifiedApAddress::v1_with_default_dp(2),
            FullyQualifiedApAddress::v1_with_default_dp(3),
        ];
        let network_access = PostEraseNetworkAccess::Restore(NetworkEraseAccess {
            application_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(0),
            network_ahb_ap: FullyQualifiedApAddress::v1_with_default_dp(1),
        });

        assert!(matches!(
            perform_debug_erase_all(
                &mut interface,
                &ctrl_aps,
                &crate::Permissions::new().allow_erase_all(),
                true,
                &network_access,
            ),
            Err(ArmError::Timeout)
        ));
        assert_eq!(
            interface
                .operations
                .iter()
                .filter_map(|operation| match operation {
                    RecoveryOperation::Write(ap, ERASEALL, 1) => Some(ap.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            [ApAddress::V1(2)]
        );
        assert!(matches!(
            interface.operations.last(),
            Some(RecoveryOperation::RestoreNetwork(
                ApAddress::V1(0),
                ApAddress::V1(1)
            ))
        ));
    }

    #[derive(Debug, PartialEq)]
    enum ForceoffOperation {
        Read(u64),
        Write(u64, u32),
        Flush,
        Sleep(Duration),
    }

    struct MockNetworkForceoffInterface {
        periph_config: u32,
        forceoff: u32,
        errata_var1: u32,
        errata_var2: u32,
        fail_write_address: Option<u64>,
        operations: Vec<ForceoffOperation>,
    }

    impl MockNetworkForceoffInterface {
        fn new(periph_config: u32, forceoff: u32, errata_var1: u32, errata_var2: u32) -> Self {
            Self {
                periph_config,
                forceoff,
                errata_var1,
                errata_var2,
                fail_write_address: None,
                operations: Vec::new(),
            }
        }
    }

    impl NetworkForceoffInterface for MockNetworkForceoffInterface {
        fn read_forceoff_word(&mut self, address: u64) -> Result<u32, ArmError> {
            self.operations.push(ForceoffOperation::Read(address));
            if address == APPLICATION_SPU_PERIPH_PERM + 0x4 * APPLICATION_RESET_PERIPH_ID {
                Ok(self.periph_config)
            } else if address == APPLICATION_FICR_ERRATA_VAR1_REGISTER {
                Ok(self.errata_var1)
            } else if address == APPLICATION_FICR_ERRATA_VAR2_REGISTER {
                Ok(self.errata_var2)
            } else {
                Ok(self.forceoff)
            }
        }

        fn write_forceoff_word(&mut self, address: u64, value: u32) -> Result<(), ArmError> {
            self.operations
                .push(ForceoffOperation::Write(address, value));
            if self.fail_write_address == Some(address) {
                self.fail_write_address = None;
                return Err(ArmError::Timeout);
            }
            if address == u64::from(APPLICATION_RESET_S_NETWORK_FORCEOFF_REGISTER)
                || address == u64::from(APPLICATION_RESET_NS_NETWORK_FORCEOFF_REGISTER)
            {
                self.forceoff = value;
            }
            Ok(())
        }

        fn flush_forceoff(&mut self) -> Result<(), ArmError> {
            self.operations.push(ForceoffOperation::Flush);
            Ok(())
        }

        fn sleep_forceoff(&mut self, duration: Duration) {
            self.operations.push(ForceoffOperation::Sleep(duration));
        }
    }

    #[test]
    fn held_secure_forceoff_is_released_and_flushed() {
        let mut interface = MockNetworkForceoffInterface::new(1 << 4, 1, 0, 0);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::Released);

        assert_eq!(
            interface.operations,
            [
                ForceoffOperation::Read(0x50003814),
                ForceoffOperation::Read(0x50005614),
                ForceoffOperation::Read(0x00ff0130),
                ForceoffOperation::Read(0x00ff0134),
                ForceoffOperation::Write(0x50005614, RELEASE_FORCEOFF),
                ForceoffOperation::Flush,
            ]
        );
    }

    #[test]
    fn held_non_secure_forceoff_uses_the_non_secure_alias() {
        let mut interface = MockNetworkForceoffInterface::new(0, 1, 0, 0);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::Released);

        assert_eq!(
            interface.operations,
            [
                ForceoffOperation::Read(0x50003814),
                ForceoffOperation::Read(0x40005614),
                ForceoffOperation::Read(0x00ff0130),
                ForceoffOperation::Read(0x00ff0134),
                ForceoffOperation::Write(0x40005614, RELEASE_FORCEOFF),
                ForceoffOperation::Flush,
            ]
        );
    }

    #[test]
    fn released_network_core_is_not_reset_or_rewritten() {
        let mut interface = MockNetworkForceoffInterface::new(1 << 4, RELEASE_FORCEOFF, 0x07, 0x05);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::AlreadyReleased);

        assert_eq!(
            interface.operations,
            [
                ForceoffOperation::Read(0x50003814),
                ForceoffOperation::Read(0x50005614),
            ]
        );
    }

    #[test]
    fn affected_secure_revision_uses_erratum_161_sequence() {
        let mut interface = MockNetworkForceoffInterface::new(1 << 4, HOLD_FORCEOFF, 0x07, 0x05);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::Released);

        assert_eq!(
            interface.operations,
            [
                ForceoffOperation::Read(0x50003814),
                ForceoffOperation::Read(0x50005614),
                ForceoffOperation::Read(0x00ff0130),
                ForceoffOperation::Read(0x00ff0134),
                ForceoffOperation::Write(0x50005618, 1),
                ForceoffOperation::Flush,
                ForceoffOperation::Write(0x50005614, RELEASE_FORCEOFF),
                ForceoffOperation::Flush,
                ForceoffOperation::Sleep(ERRATUM_161_RELEASE_DELAY),
                ForceoffOperation::Write(0x50005614, HOLD_FORCEOFF),
                ForceoffOperation::Flush,
                ForceoffOperation::Sleep(ERRATUM_161_HOLD_DELAY),
                ForceoffOperation::Write(0x50005614, RELEASE_FORCEOFF),
                ForceoffOperation::Flush,
                ForceoffOperation::Write(0x50005618, 0),
                ForceoffOperation::Flush,
            ]
        );
    }

    #[test]
    fn affected_non_secure_revision_uses_matching_aliases() {
        let mut interface = MockNetworkForceoffInterface::new(0, HOLD_FORCEOFF, 0x07, 0x05);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::Released);

        assert!(
            interface
                .operations
                .contains(&ForceoffOperation::Write(0x40005618, 1))
        );
        assert!(
            interface
                .operations
                .contains(&ForceoffOperation::Write(0x40005614, RELEASE_FORCEOFF))
        );
        assert!(!interface.operations.iter().any(|operation| matches!(
            operation,
            ForceoffOperation::Write(address, _) if *address >= 0x50005614
        )));
    }

    #[test]
    fn erratum_failure_attempts_to_hold_core_and_clear_workaround() {
        let mut interface = MockNetworkForceoffInterface::new(1 << 4, HOLD_FORCEOFF, 0x07, 0x05);
        interface.fail_write_address = Some(0x50005614);

        assert!(set_network_core_running(&mut interface).is_err());

        assert!(
            interface
                .operations
                .contains(&ForceoffOperation::Write(0x50005614, HOLD_FORCEOFF))
        );
        assert!(
            interface
                .operations
                .contains(&ForceoffOperation::Write(0x50005618, 0))
        );
    }

    #[test]
    fn unaffected_revision_one_variant_uses_plain_release() {
        let mut interface = MockNetworkForceoffInterface::new(1 << 4, HOLD_FORCEOFF, 0x07, 0x04);

        let release = set_network_core_running(&mut interface).unwrap();

        assert_eq!(release, NetworkCoreRelease::Released);
        assert!(!interface.operations.iter().any(|operation| matches!(
            operation,
            ForceoffOperation::Write(address, _) if *address == 0x50005618
        )));
    }
}
