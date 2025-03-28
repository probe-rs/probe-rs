//! Debug sequences to operate special requirements ARM targets.

use std::{
    error::Error,
    fmt::Debug,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::CoreType;

use crate::{
    MemoryInterface, MemoryMappedRegister, Session,
    architecture::arm::{
        ArmProbeInterface, DapError, RegisterAddress,
        core::registers::cortex_m::{PC, SP},
        dp::{Ctrl, DLPIDR, DebugPortError, DpRegister, TARGETID},
    },
    probe::WireProtocol,
};

use super::{
    ArmError, DapAccess, FullyQualifiedApAddress, Pins,
    ap::AccessPortError,
    armv6m::Demcr,
    communication_interface::DapProbe,
    component::{TraceFunnel, TraceSink},
    core::cortex_m::{Dhcsr, Vtor},
    dp::{Abort, DPIDR, DpAccess, DpAddress, SelectV1},
    memory::{
        ArmMemoryInterface,
        romtable::{CoresightComponent, PeripheralType},
    },
};

/// An error occurred when executing an ARM debug sequence
#[derive(thiserror::Error, Debug)]
pub enum ArmDebugSequenceError {
    /// Debug base address is required but not specified
    #[error("Core access requries debug_base to be specified, but it is not")]
    DebugBaseNotSpecified,

    /// CTI base address is required but not specified
    #[error("Core access requries cti_base to be specified, but it is not")]
    CtiBaseNotSpecified,

    /// An error occurred in a debug sequence.
    #[error("An error occurred in a debug sequnce: {0}")]
    SequenceSpecific(#[from] Box<dyn Error + Send + Sync + 'static>),
}

impl ArmDebugSequenceError {
    pub(crate) fn custom(message: impl Into<Box<dyn Error + Send + Sync + 'static>>) -> Self {
        ArmDebugSequenceError::SequenceSpecific(message.into())
    }
}

/// The default sequences that is used for ARM chips that do not specify a specific sequence.
#[derive(Debug)]
pub struct DefaultArmSequence(pub(crate) ());

impl DefaultArmSequence {
    /// Creates a new default ARM debug sequence.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for DefaultArmSequence {}

/// ResetCatchSet for Cortex-A devices
fn armv7a_reset_catch_set(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::Dbgprcr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(core.read_word_32(address)?);

    dbgprcr.set_hcwr(true);

    core.write_word_32(address, dbgprcr.into())?;

    Ok(())
}

/// ResetCatchClear for Cortex-A devices
fn armv7a_reset_catch_clear(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::Dbgprcr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(core.read_word_32(address)?);

    dbgprcr.set_hcwr(false);

    core.write_word_32(address, dbgprcr.into())?;

    Ok(())
}

fn armv7a_reset_system(
    interface: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::{Dbgprcr, Dbgprsr};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    // Request reset
    let address = Dbgprcr::get_mmio_address_from_base(debug_base)?;
    let mut dbgprcr = Dbgprcr(interface.read_word_32(address)?);

    dbgprcr.set_cwrr(true);

    interface.write_word_32(address, dbgprcr.into())?;

    // Wait until reset happens
    let address = Dbgprsr::get_mmio_address_from_base(debug_base)?;

    loop {
        let dbgprsr = Dbgprsr(interface.read_word_32(address)?);
        if dbgprsr.sr() {
            break;
        }
    }

    Ok(())
}

/// DebugCoreStart for v7 Cortex-A devices
fn armv7a_core_start(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7a_debug_regs::{Dbgdsccr, Dbgdscr, Dbgdsmcr, Dbglar};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;
    tracing::debug!(
        "Starting debug for ARMv7-A core with registers at {:#X}",
        debug_base
    );

    // Lock OS register access to prevent race conditions
    let address = Dbglar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbglar(0).into())?;

    // Force write through / disable caching for debugger access
    let address = Dbgdsccr::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbgdsccr(0).into())?;

    // Disable TLB matching and updates for debugger operations
    let address = Dbgdsmcr::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Dbgdsmcr(0).into())?;

    // Enable halting
    let address = Dbgdscr::get_mmio_address_from_base(debug_base)?;
    let mut dbgdscr = Dbgdscr(core.read_word_32(address)?);

    if dbgdscr.hdbgen() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }

    dbgdscr.set_hdbgen(true);
    core.write_word_32(address, dbgdscr.into())?;

    Ok(())
}

/// ResetCatchSet for ARMv8-A devices
fn armv8a_reset_catch_set(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::Edecr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Edecr::get_mmio_address_from_base(debug_base)?;
    let mut edecr = Edecr(core.read_word_32(address)?);

    edecr.set_rce(true);

    core.write_word_32(address, edecr.into())?;

    Ok(())
}

/// ResetCatchClear for ARMv8-a devices
fn armv8a_reset_catch_clear(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::Edecr;

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    let address = Edecr::get_mmio_address_from_base(debug_base)?;
    let mut edecr = Edecr(core.read_word_32(address)?);

    edecr.set_rce(false);

    core.write_word_32(address, edecr.into())?;

    Ok(())
}

fn armv8a_reset_system(
    interface: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::{Edprcr, Edprsr};

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;

    // Request reset
    let address = Edprcr::get_mmio_address_from_base(debug_base)?;
    let mut edprcr = Edprcr(interface.read_word_32(address)?);

    edprcr.set_cwrr(true);

    interface.write_word_32(address, edprcr.into())?;

    // Wait until reset happens
    let address = Edprsr::get_mmio_address_from_base(debug_base)?;

    loop {
        let edprsr = Edprsr(interface.read_word_32(address)?);
        if edprsr.sr() {
            break;
        }
    }

    Ok(())
}

/// DebugCoreStart for v8 Cortex-A devices
fn armv8a_core_start(
    core: &mut dyn ArmMemoryInterface,
    debug_base: Option<u64>,
    cti_base: Option<u64>,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv8a_debug_regs::{
        CtiControl, CtiGate, CtiOuten, Edlar, Edscr, Oslar,
    };

    let debug_base =
        debug_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::DebugBaseNotSpecified))?;
    let cti_base =
        cti_base.ok_or_else(|| ArmError::from(ArmDebugSequenceError::CtiBaseNotSpecified))?;

    tracing::debug!(
        "Starting debug for ARMv8-A core with registers at {:#X}",
        debug_base
    );

    // Lock OS register access to prevent race conditions
    let address = Edlar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Edlar(0).into())?;

    // Unlock the OS Lock to enable access to debug registers
    let address = Oslar::get_mmio_address_from_base(debug_base)?;
    core.write_word_32(address, Oslar(0).into())?;

    // Configure CTI
    let mut cticontrol = CtiControl(0);
    cticontrol.set_glben(true);

    let address = CtiControl::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, cticontrol.into())?;

    // Gate all events by default
    let address = CtiGate::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, 0)?;

    // Configure output channels for halt and resume
    // Channel 0 - halt requests
    let mut ctiouten = CtiOuten(0);
    ctiouten.set_outen(0, 1);

    let address = CtiOuten::get_mmio_address_from_base(cti_base)?;
    core.write_word_32(address, ctiouten.into())?;

    // Channel 1 - resume requests
    let mut ctiouten = CtiOuten(0);
    ctiouten.set_outen(1, 1);

    let address = CtiOuten::get_mmio_address_from_base(cti_base)? + 4;
    core.write_word_32(address, ctiouten.into())?;

    // Enable halting
    let address = Edscr::get_mmio_address_from_base(debug_base)?;
    let mut edscr = Edscr(core.read_word_32(address)?);

    if edscr.hde() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }

    edscr.set_hde(true);
    core.write_word_32(address, edscr.into())?;

    Ok(())
}

/// DebugCoreStart for Cortex-M devices
pub(crate) fn cortex_m_core_start(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Dhcsr;

    let current_dhcsr = Dhcsr(core.read_word_32(Dhcsr::get_mmio_address())?);

    // Note: Manual addition for debugging, not part of the original DebugCoreStart function
    if current_dhcsr.c_debugen() {
        tracing::debug!("Core is already in debug mode, no need to enable it again");
        return Ok(());
    }
    // -- End addition

    let mut dhcsr = Dhcsr(0);
    dhcsr.set_c_debugen(true);
    dhcsr.enable_write();

    core.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;

    Ok(())
}

/// ResetCatchClear for Cortex-M devices
fn cortex_m_reset_catch_clear(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Demcr;

    // Clear reset catch bit
    let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
    demcr.set_vc_corereset(false);

    core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
    Ok(())
}

/// ResetCatchSet for Cortex-M devices
fn cortex_m_reset_catch_set(core: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::{Demcr, Dhcsr};

    // Request halt after reset
    let mut demcr = Demcr(core.read_word_32(Demcr::get_mmio_address())?);
    demcr.set_vc_corereset(true);

    core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

    // Clear the status bits by reading from DHCSR
    let _ = core.read_word_32(Dhcsr::get_mmio_address())?;

    Ok(())
}

/// ResetSystem for Cortex-M devices
fn cortex_m_reset_system(interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Aircr;

    let mut aircr = Aircr(0);
    aircr.vectkey();
    aircr.set_sysresetreq(true);

    interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;

    cortex_m_wait_for_reset(interface)
}

/// Wait for Cortex-M device to reset
pub(crate) fn cortex_m_wait_for_reset(
    interface: &mut dyn ArmMemoryInterface,
) -> Result<(), ArmError> {
    use crate::architecture::arm::core::armv7m::Dhcsr;

    let start = Instant::now();

    // PSOC 6 documentation states 600ms is the maximum possible time
    // before the debug port becomes available again after reset
    while start.elapsed() < Duration::from_millis(600) {
        let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
            // Some combinations of debug probe and target (in
            // particular, hs-probe and ATSAMD21) result in
            // register read errors while the target is
            // resetting.
            Ok(val) => Dhcsr(val),
            Err(ArmError::AccessPort {
                source: AccessPortError::RegisterRead { source, .. },
                ..
            }) => {
                if let Some(ArmError::Dap(DapError::NoAcknowledge)) =
                    source.downcast_ref::<ArmError>()
                {
                    // On PSOC 6, a system reset resets the SWD interface as well,
                    // so we have to reinitialize.
                    if let Ok(probe) = interface.get_arm_probe_interface() {
                        probe.reinitialize()?;
                    }
                }
                continue;
            }
            Err(err) => return Err(err),
        };
        if !dhcsr.s_reset_st() {
            return Ok(());
        }
    }

    Err(ArmError::Timeout)
}

/// A interface to operate debug sequences for ARM targets.
///
/// Should be implemented on a custom handle for chips that require special sequence code.
pub trait ArmDebugSequence: Send + Sync + Debug {
    /// Assert a system-wide reset line nRST. This is based on the
    /// `ResetHardwareAssert` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#resetHardwareAssert
    #[doc(alias = "ResetHardwareAssert")]
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);

        let _ = interface.swj_pins(0, n_reset.0 as u32, 0)?;

        Ok(())
    }

    /// De-Assert a system-wide reset line nRST. This is based on the
    /// `ResetHardwareDeassert` function from the [ARM SVD Debug Description].
    ///
    /// This function is called _once_ when using the "connect under reset" functionality,
    /// after a connection to each core has been established, and it is expected that after
    /// this function is called, the target will start executing code.
    ///
    /// The provided `default_ap` is the default access port that should be used for memory
    /// access, if the sequence requires it.
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#resetHardwareDeassert
    #[doc(alias = "ResetHardwareDeassert")]
    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmProbeInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        let can_read_pins = probe.swj_pins(n_reset, n_reset, 0)? != 0xffff_ffff;

        if can_read_pins {
            let start = Instant::now();

            loop {
                if Pins(probe.swj_pins(n_reset, n_reset, 0)? as u8).nreset() {
                    return Ok(());
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
                thread::sleep(Duration::from_millis(100));
            }
        } else {
            thread::sleep(Duration::from_millis(100));
            Ok(())
        }
    }

    /// Prepare the target debug port for connection. This is based on the `DebugPortSetup` function
    /// from the [ARM SVD Debug Description].
    ///
    /// After this function has been executed, it should be possible to read and write registers
    /// using SWD requests.
    ///
    /// If this function cannot read the DPIDR register, it will retry up to 5 times, and return an
    /// error if it still cannot read it.
    ///
    /// [ARM SVD Debug Description]:
    ///     https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugPortSetup
    #[doc(alias = "DebugPortSetup")]
    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // TODO: Handle this differently for ST-Link?
        tracing::debug!("Setting up debug port {dp:x?}");

        // A multidrop address implies SWD version 2 and dormant state.  In
        // cases where SWD version 2 is used but not multidrop addressing
        // (ex. ADIv6), the SWD version 1 sequence is attempted before trying
        // the SWD version 2 sequence.
        let mut has_dormant = matches!(dp, DpAddress::Multidrop(_));

        fn alert_sequence(interface: &mut dyn DapProbe) -> Result<(), ArmError> {
            tracing::trace!("Sending Selection Alert sequence");

            // Ensure target is not in the middle of detecting a selection alert
            interface.swj_sequence(8, 0xFF)?;

            // Alert Sequence Bits  0.. 63
            interface.swj_sequence(64, 0x86852D956209F392)?;

            // Alert Sequence Bits 64..127
            interface.swj_sequence(64, 0x19BC0EA2E3DDAFE9)?;

            Ok(())
        }

        // TODO: Use atomic block

        let mut result = Ok(());
        const NUM_RETRIES: usize = 5;
        for retry in 0..NUM_RETRIES {
            // Ensure current debug interface is in reset state.
            swd_line_reset(interface, 0)?;

            // Make sure the debug port is in the correct mode based on what the probe
            // has selected via active_protocol
            match interface.active_protocol() {
                Some(WireProtocol::Jtag) => {
                    if has_dormant {
                        tracing::debug!("Select Dormant State (from SWD)");
                        interface.swj_sequence(16, 0xE3BC)?;

                        // Send alert sequence
                        alert_sequence(interface)?;

                        // 4 cycles SWDIO/TMS LOW + 8-Bit JTAG Activation Code (0x0A)
                        interface.swj_sequence(12, 0x0A0)?;
                    } else {
                        // Execute SWJ-DP Switch Sequence SWD to JTAG (0xE73C).
                        interface.swj_sequence(16, 0xE73C)?;
                    }

                    // Execute at least >5 TCK cycles with TMS high to enter the Test-Logic-Reset state
                    interface.swj_sequence(6, 0x3F)?;

                    // Enter Run-Test-Idle state, as required by the DAP_Transfer command when using JTAG
                    interface.jtag_sequence(1, false, 0x01)?;

                    // Configure JTAG IR lengths in probe
                    interface.configure_jtag(false)?;
                }
                Some(WireProtocol::Swd) => {
                    if has_dormant {
                        // Select Dormant State (from JTAG)
                        tracing::debug!("SelectV1 Dormant State (from JTAG)");
                        interface.swj_sequence(31, 0x33BBBBBA)?;

                        // Leave dormant state
                        alert_sequence(interface)?;

                        // 4 cycles SWDIO/TMS LOW + 8-Bit SWD Activation Code (0x1A)
                        interface.swj_sequence(12, 0x1A0)?;
                    } else {
                        // Execute SWJ-DP Switch Sequence JTAG to SWD (0xE79E).
                        // Change if SWJ-DP uses deprecated switch code (0xEDB6).
                        interface.swj_sequence(16, 0xE79E)?;

                        // > 50 cycles SWDIO/TMS High, at least 2 idle cycles (SWDIO/TMS Low).
                        // -> done in debug_port_connect
                    }
                }
                _ => {
                    return Err(ArmDebugSequenceError::SequenceSpecific(
                        "Cannot detect current protocol".into(),
                    )
                    .into());
                }
            }

            // End of atomic block.

            // SWD or JTAG should now be activated, so we can try and connect to the debug port.
            result = self.debug_port_connect(interface, dp);
            if result.is_ok() {
                // Successful connection, we can stop retrying.
                break;
            }

            // If two retries have failed, try using SWD version 2 wake from
            // dormant sequence.
            if retry >= 1 {
                has_dormant = true;
            }
        }

        result
    }

    /// Connect to the target debug port and power it up. This is based on the
    /// `DebugPortStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugPortStart
    #[doc(alias = "DebugPortStart")]
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        interface.write_dp_register(dp, SelectV1(0))?;
        let dpidr: DPIDR = interface.read_dp_register(dp)?;

        // Clear all errors and see if we need to power up the target
        let ctrl = interface.read_dp_register::<Ctrl>(dp)?;
        let powered_down = if dpidr.version() == 0 || dpidr.designer() == 0 {
            // Abort the current transaction. This is the only bit present in `Abort` in `DPv0`.
            let mut abort = Abort(0);
            abort.set_dapabort(true);
            interface.write_dp_register(dp, abort)?;

            // To clear errors on DPv0, we need to set W1C values for STICKYORUN, STICKYCMP, and STICKYERR
            interface.write_dp_register::<Ctrl>(dp, Ctrl(0x32))?;

            // DPv0 always needs to be powered up
            true
        } else {
            // CMSIS says this is only necessary to do inside the `if powered_down`, but
            // without it here, nRF52840 faults in the next access.
            let mut abort = Abort(0);
            abort.set_dapabort(true);
            abort.set_orunerrclr(true);
            abort.set_wderrclr(true);
            abort.set_stkerrclr(true);
            abort.set_stkcmpclr(true);
            interface.write_dp_register(dp, abort)?;

            !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack())
        };

        if powered_down {
            tracing::info!("Debug port {dp:x?} is powered down, powering up");
            let mut ctrl = Ctrl(0);
            ctrl.set_cdbgpwrupreq(true);
            ctrl.set_csyspwrupreq(true);
            if !dpidr.min() {
                // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode,
                // Include all Byte Lanes).
                //
                // Setting this on MINDP is unpredictable.
                ctrl.set_mask_lane(0b1111);
            }

            match interface
                .write_dp_register(dp, ctrl.clone())
                .and_then(|_| interface.flush())
            {
                Ok(()) => {}
                Err(e @ ArmError::Dap(DapError::NoAcknowledge)) => {
                    // If we get a NACK from the power-up request, ignore the error & perform a line reset.
                    // (CMSIS-DAP transports read DP.RDBUFF right after a write to DP.CTRL_STAT.
                    //  This fails in some cases on PSOC 6, for example if the device is waking from DeepSleep.
                    //  If something really went wrong, we'll hit an error or timeout in the polling loop below.)
                    let Some(probe) = interface.try_dap_probe_mut() else {
                        tracing::warn!(
                            "Power-up request returned NACK, but we don't have a DapProbe, so we can't reconnect"
                        );
                        return Err(e);
                    };
                    tracing::info!("Power-up request returned NACK, reconnecting");
                    self.debug_port_connect(probe, dp)?;
                }
                Err(e) => return Err(e),
            }

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

            // TODO: Handle JTAG Specific part

            // TODO: Only run the following code when the SWD protocol is used

            // Write the CTRL register after powerup completes. It's unknown if this is required
            // for all devices, but there's little harm in writing the same value back to the
            // register after powerup.
            interface.write_dp_register(dp, ctrl)?;

            let ctrl_reg: Ctrl = interface.read_dp_register(dp)?;
            if !(ctrl_reg.csyspwrupack() && ctrl_reg.cdbgpwrupack()) {
                tracing::error!("Debug power request failed");
                return Err(DebugPortError::TargetPowerUpFailed.into());
            }

            // According to CMSIS docs, here's where we would clear errors
            // in ABORT, but we do that above instead.
        }

        Ok(())
    }

    /// Initialize core debug system. This is based on the
    /// `DebugCoreStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugCoreStart
    #[doc(alias = "DebugCoreStart")]
    fn debug_core_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        core_ap: &FullyQualifiedApAddress,
        core_type: CoreType,
        debug_base: Option<u64>,
        cti_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut core = interface.memory_interface(core_ap)?;

        // Dispatch based on core type (Cortex-A vs M)
        match core_type {
            CoreType::Armv7a => armv7a_core_start(&mut *core, debug_base),
            CoreType::Armv8a => armv8a_core_start(&mut *core, debug_base, cti_base),
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m => {
                cortex_m_core_start(&mut *core)
            }
            _ => panic!("Logic inconsistency bug - non ARM core type passed {core_type:?}"),
        }
    }

    /// Configure the target to stop code execution after a reset. After this, the core will halt when it comes
    /// out of reset. This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#resetCatchSet
    #[doc(alias = "ResetCatchSet")]
    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Dispatch based on core type (Cortex-A vs M)
        match core_type {
            CoreType::Armv7a => armv7a_reset_catch_set(core, debug_base),
            CoreType::Armv8a => armv8a_reset_catch_set(core, debug_base),
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m => {
                cortex_m_reset_catch_set(core)
            }
            _ => panic!("Logic inconsistency bug - non ARM core type passed {core_type:?}"),
        }
    }

    /// Free hardware resources allocated by ResetCatchSet.
    /// This is based on the `ResetCatchSet` function from
    /// the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#resetCatchClear
    #[doc(alias = "ResetCatchClear")]
    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Dispatch based on core type (Cortex-A vs M)
        match core_type {
            CoreType::Armv7a => armv7a_reset_catch_clear(core, debug_base),
            CoreType::Armv8a => armv8a_reset_catch_clear(core, debug_base),
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m => {
                cortex_m_reset_catch_clear(core)
            }
            _ => panic!("Logic inconsistency bug - non ARM core type passed {core_type:?}"),
        }
    }

    /// Enable target trace capture.
    ///
    /// # Note
    /// This function is responsible for configuring any of the CoreSight link components, such as
    /// trace funnels, to route trace data to the specified trace sink.
    ///
    /// This is based on the `TraceStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#traceStart
    fn trace_start(
        &self,
        interface: &mut dyn ArmProbeInterface,
        components: &[CoresightComponent],
        _sink: &TraceSink,
    ) -> Result<(), ArmError> {
        // As a default implementation, enable all of the slave port inputs of any trace funnels
        // found. This should enable _all_ sinks simultaneously. Device-specific implementations
        // can be written to properly configure the specified sink.
        for trace_funnel in components
            .iter()
            .filter_map(|comp| comp.find_component(PeripheralType::TraceFunnel))
        {
            let mut funnel = TraceFunnel::new(interface, trace_funnel);
            funnel.unlock()?;
            funnel.enable_port(0xFF)?;
        }

        Ok(())
    }

    /// Executes a system-wide reset without debug domain (or warm-reset that preserves debug connection) via software mechanisms,
    /// for example AIRCR.SYSRESETREQ.  This is based on the
    /// `ResetSystem` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#resetSystem
    #[doc(alias = "ResetSystem")]
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Dispatch based on core type (Cortex-A vs M)
        match core_type {
            CoreType::Armv7a => armv7a_reset_system(interface, debug_base),
            CoreType::Armv8a => armv8a_reset_system(interface, debug_base),
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m => {
                cortex_m_reset_system(interface)
            }
            _ => panic!("Logic inconsistency bug - non ARM core type passed {core_type:?}"),
        }
    }

    /// Check if the device is in a locked state and unlock it.
    /// Use query command elements for user confirmation.
    /// Executed after having powered up the debug port. This is based on the
    /// `DebugDeviceUnlock` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugDeviceUnlock
    #[doc(alias = "DebugDeviceUnlock")]
    fn debug_device_unlock(
        &self,
        _interface: &mut dyn ArmProbeInterface,
        _default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        tracing::debug!("debug_device_unlock - empty by default");
        Ok(())
    }

    /// Executed before step or run command to support recovery from a lost target connection, e.g. after a low power mode.
    /// This is based on the `RecoverSupportStart` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.htmll#recoverSupportStart
    #[doc(alias = "RecoverSupportStart")]
    fn recover_support_start(
        &self,
        _interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        // Empty by default
        Ok(())
    }

    /// Executed when the debugger session is disconnected from the core.
    ///
    /// This is based on the `DebugCoreStop` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugCoreStop
    #[doc(alias = "DebugCoreStop")]
    fn debug_core_stop(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        if core_type.is_cortex_m() {
            // System Control Space (SCS) offset as defined in Armv6-M/Armv7-M.
            // Disable Core Debug via DHCSR
            let mut dhcsr = Dhcsr(0);
            dhcsr.enable_write();
            interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.0)?;

            // Disable DWT and ITM blocks, DebugMonitor handler,
            // halting debug traps, and Reset Vector Catch.
            interface.write_word_32(Demcr::get_mmio_address(), 0x0)?;
        }

        Ok(())
    }

    /// Sequence executed when disconnecting from a debug port.
    ///
    /// Based on the `DebugPortStop` function from the [ARM SVD Debug Description].
    ///
    /// [ARM SVD Debug Description]: https://open-cmsis-pack.github.io/Open-CMSIS-Pack-Spec/main/html/debug_description.html#debugPortStop
    #[doc(alias = "DebugPortStop")]
    fn debug_port_stop(&self, interface: &mut dyn DapProbe, dp: DpAddress) -> Result<(), ArmError> {
        tracing::info!("Powering down debug port {dp:x?}");
        // Select Bank 0
        interface.raw_write_register(SelectV1::ADDRESS.into(), 0)?;

        // De-assert debug power request
        interface.raw_write_register(Ctrl::ADDRESS.into(), 0)?;

        // Wait for the power domains to go away
        let start = Instant::now();
        loop {
            let ctrl = interface.raw_read_register(Ctrl::ADDRESS.into())?;
            let ctrl = Ctrl(ctrl);
            if !(ctrl.csyspwrupack() || ctrl.cdbgpwrupack()) {
                return Ok(());
            }

            if start.elapsed() >= Duration::from_secs(1) {
                return Err(ArmError::Timeout);
            }
        }
    }

    /// Perform a SWD line reset or enter the JTAG Run-Test-Idle state, and then try to connect to a debug port.
    ///
    /// This is executed as part of the standard `debug_port_setup` sequence,
    /// and when switching between debug ports in a SWD multi-drop configuration.
    ///
    /// If the `dp` parameter is `DpAddress::Default`, a read of the DPIDR register will be
    /// performed after the line reset.
    ///
    /// If the `dp` parameter is `DpAddress::Multidrop`, a write of the `TARGETSEL` register is
    /// done after the line reset, followed by a read of the `DPIDR` register.
    ///
    /// This is not based on a sequence from the Open-CMSIS-Pack standard.
    #[tracing::instrument(level = "debug", skip_all)]
    fn debug_port_connect(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        match interface.active_protocol() {
            Some(WireProtocol::Jtag) => {
                tracing::debug!("JTAG: No special sequence needed to connect to debug port");
                return Ok(());
            }
            Some(WireProtocol::Swd) => {
                tracing::debug!("SWD: Connecting to debug port with address {:x?}", dp);
            }
            None => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        // Time guard for DPIDR register to become readable after line reset
        const RESET_RECOVERY_TIMEOUT: Duration = Duration::from_secs(1);
        const RESET_RECOVERY_RETRY_INTERVAL: Duration = Duration::from_millis(5);

        // Enter SWD Line Reset State, afterwards at least 2 idle cycles (SWDIO/TMS Low)
        // Guard gives time for the target to recover
        let guard = Instant::now();
        let dpidr = loop {
            swd_line_reset(interface, 3)?;

            // If multidrop is used, we now have to select a target
            if let DpAddress::Multidrop(targetsel) = dp {
                // Deselect other debug ports first?

                tracing::debug!("Writing targetsel {:#x}", targetsel);
                // TARGETSEL write.
                // The TARGETSEL write is not ACKed by design. We can't use a normal register write
                // because many probes don't even send the data phase when NAK.
                let parity = targetsel.count_ones() % 2;
                let data = ((parity as u64) << 45) | ((targetsel as u64) << 13) | 0x1f99;

                // Should this be a swd_sequence?
                // Technically we shouldn't drive SWDIO all the time when sending a request.
                interface.swj_sequence(6 * 8, data)?;
            }

            tracing::debug!("Reading DPIDR to enable SWD interface");

            // Read DPIDR to enable SWD interface.
            match interface.raw_read_register(RegisterAddress::DpRegister(DPIDR::ADDRESS)) {
                Ok(x) => break x,
                Err(z) => {
                    if guard.elapsed() > RESET_RECOVERY_TIMEOUT {
                        tracing::debug!("DPIDR didn't become readable within guard time");
                        return Err(z);
                    }
                }
            }

            // Be nice - checking at intervals is plenty
            std::thread::sleep(RESET_RECOVERY_RETRY_INTERVAL);
        };
        tracing::debug!(
            "DPIDR became readable after {}ms",
            guard.elapsed().as_millis()
        );
        tracing::debug!("Result of DPIDR read: {:#x?}", dpidr);

        tracing::debug!("Clearing errors using ABORT register");
        let mut abort = Abort(0);
        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);

        // DPBANKSEL does not matter for ABORT
        interface.raw_write_register(Abort::ADDRESS.into(), abort.0)?;
        interface.raw_flush()?;

        // Check that we are connected to the right DP

        if let DpAddress::Multidrop(targetsel) = dp {
            tracing::debug!("Checking TARGETID and DLPIDR match");
            // Select DP Bank 2
            interface.raw_write_register(SelectV1::ADDRESS.into(), 2)?;

            let target_id = interface.raw_read_register(TARGETID::ADDRESS.into())?;

            // Select DP Bank 3
            interface.raw_write_register(SelectV1::ADDRESS.into(), 3)?;
            let dlpidr = interface.raw_read_register(DLPIDR::ADDRESS.into())?;

            const TARGETID_MASK: u32 = 0x0FFF_FFFF;
            const DLPIDR_MASK: u32 = 0xF000_0000;

            let targetid_match = (target_id & TARGETID_MASK) == (targetsel & TARGETID_MASK);
            let dlpdir_match = (dlpidr & DLPIDR_MASK) == (targetsel & DLPIDR_MASK);

            if !(targetid_match && dlpdir_match) {
                tracing::warn!(
                    "Target ID and DLPIDR do not match, failed to select debug port. Target ID: {:#x?}, DLPIDR: {:#x?}",
                    target_id,
                    dlpidr
                );
                return Err(ArmError::Other(
                    "Target ID and DLPIDR do not match, failed to select debug port".to_string(),
                ));
            }
        }

        interface.raw_write_register(SelectV1::ADDRESS.into(), 0)?;
        let ctrl_stat = interface.raw_read_register(Ctrl::ADDRESS.into()).map(Ctrl);

        match ctrl_stat {
            Ok(ctrl_stat) => {
                tracing::debug!("Result of CTRL/STAT read: {:?}", ctrl_stat);
            }
            Err(e) => {
                // According to the SPEC, reading from CTRL/STAT should never fail. In practice,
                // it seems to fail sometimes.
                tracing::debug!("Failed to read CTRL/STAT: {:?}", e);
            }
        }

        Ok(())
    }

    /// This ARM sequence is called if an image was flashed to RAM directly.
    /// It will perform the necessary preparation to run that image.
    ///
    /// Core should be already `reset_and_halt`ed right before this call.
    fn prepare_running_on_ram(
        &self,
        vector_table_addr: u64,
        session: &mut Session,
    ) -> Result<(), crate::Error> {
        tracing::info!("Performing RAM flash start");
        const SP_MAIN_OFFSET: usize = 0;
        const RESET_VECTOR_OFFSET: usize = 1;

        if session.list_cores().len() > 1 {
            return Err(crate::Error::NotImplemented(
                "multi-core ram flash start not implemented yet",
            ));
        }

        let (_, core_type) = session.list_cores()[0];
        match core_type {
            CoreType::Armv7a | CoreType::Armv8a => {
                return Err(crate::Error::NotImplemented(
                    "RAM flash not implemented for ARM Cortex-A",
                ));
            }
            CoreType::Armv6m | CoreType::Armv7m | CoreType::Armv7em | CoreType::Armv8m => {
                tracing::debug!("RAM flash start for Cortex-M single core target");
                let mut core = session.core(0)?;
                // See ARMv7-M Architecture Reference Manual Chapter B1.5 for more details. The
                // process appears to be the same for the other Cortex-M architectures as well.
                let vtor = Vtor(vector_table_addr as u32);
                let mut first_table_entries: [u32; 2] = [0; 2];
                core.read_32(vector_table_addr, &mut first_table_entries)?;
                // The first entry in the vector table is the SP_main reset value of the main stack pointer,
                // so we set the stack pointer register accordingly.
                core.write_core_reg(SP.id, first_table_entries[SP_MAIN_OFFSET])?;
                // The second entry in the vector table is the reset vector. It needs to be loaded
                // as the initial PC value on a reset, see chapter A2.3.1 of the reference manual.
                core.write_core_reg(PC.id, first_table_entries[RESET_VECTOR_OFFSET])?;
                core.write_word_32(Vtor::get_mmio_address(), vtor.0)?;
            }
            _ => {
                panic!("Logic inconsistency bug - non ARM core type passed {core_type:?}");
            }
        }
        Ok(())
    }

    /// Return the Debug Erase Sequence implementation if it exists
    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        None
    }

    /// Return the APs that are expected to work.
    fn allowed_access_ports(&self) -> Vec<u8> {
        (0..=255).collect()
    }
}

/// Chip-Erase Handling via the Device's Debug Interface
pub trait DebugEraseSequence: Send + Sync {
    /// Perform Chip-Erase by vendor specific means.
    ///
    /// Some devices provide custom methods for mass erasing the entire flash area and even reset
    /// other non-volatile chip state to its default setting.
    ///
    /// # Errors
    /// May fail if the device is e.g. permanently locked or due to communication issues with the device.
    /// Some devices require the probe to be disconnected and re-attached after a successful chip-erase in
    /// which case it will return `Error::Probe(DebugProbeError::ReAttachRequired)`
    fn erase_all(&self, _interface: &mut dyn ArmProbeInterface) -> Result<(), ArmError> {
        Err(ArmError::NotImplemented("erase_all"))
    }
}

/// Perform a SWD line reset (SWDIO high for 50 clock cycles)
///
/// After the line reset, SWDIO will be kept low for `swdio_low_cycles` cycles.
fn swd_line_reset(interface: &mut dyn DapProbe, swdio_low_cycles: u8) -> Result<(), ArmError> {
    assert!(swdio_low_cycles + 51 <= 64);

    tracing::debug!("Performing SWD line reset");
    interface.swj_sequence(51 + swdio_low_cycles, 0x0007_FFFF_FFFF_FFFF)?;

    Ok(())
}
