//! Sequences for TI MSPM0 devices.
//!
//! Documentation of the debug subsystem is available in "Hardware Programming and Debugger Guide for
//! MSPM0" (<https://www.ti.com/lit/an/slaaeo5/slaaeo5.pdf>)
//!
//! The MSPM0's AHB-AP lives in power domain PD1. Entering DEEPSLEEP (STOP or STANDBY) disables
//! PD1, which makes the AHB-AP undiscoverable and drops the debug session. This is not limited to
//! applications that sleep deliberately: a blank device parks itself in STANDBY0 after roughly ten
//! seconds of bootcode, which revokes the AHB-AP the same way.
//!
//! The override is the PWR-AP (APSEL 4). Setting `INHIBITSLEEP` and `FORCEACTIVE` in its `DPREC0`
//! register forces the device out of low-power mode and keeps it out for as long as the debugger
//! is attached. TI documents this as mandatory when adding MSPM0 support ("Hardware Programming
//! and Debugger Guide for MSPM0", SLAAEO5 section 3.1).
//!
//! The values here are transcribed from TI's own low-power-mode patches shipped in the MSPM0 SDK
//! (`tools/keil/low_power_mode_patch/*.pdsc` `DebugPortStart` sequences, cross-checked against
//! `tools/iar/low_power_mode_patch/*.dmac` `_InhibitSleepForceActive()`).

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::Permissions;
use crate::architecture::arm::ap::{AccessPortError, ApRegister, IDR};
use crate::architecture::arm::dp::DpAddress;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{
    ArmDebugSequence, DebugEraseSequence, cortex_m_wait_for_reset,
};
use crate::architecture::arm::{ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress};
use crate::probe::Pins;
use crate::session::MissingPermissions;
use probe_rs_target::CoreType;

/// Access Port Select values used by this sequence.
#[derive(Debug, Clone, Copy)]
enum ApSel {
    /// SEC-AP: security access port.
    SecAp = 2,

    /// PWR-AP: controls the power and reset state of the CPU for debug purposes.
    PwrAp = 4,
}

impl From<ApSel> for FullyQualifiedApAddress {
    fn from(apsel: ApSel) -> Self {
        FullyQualifiedApAddress::v1_with_default_dp(apsel as u8)
    }
}

/// DSSM commands processed that can be sent to the SEC-AP.
#[repr(u16)]
#[derive(Debug, Clone, Copy)]
enum DssmCommand {
    FactoryReset = 0x020A,

    MassErase = 0x020C,
}

/// Debug power and reset control register, PWR-AP register bank 0.
const DPREC0: u64 = 0x00;
/// System power and reset control register, PWR-AP register bank 15.
///
/// The AP bank is derived from the register address by the communication interface, so this can be
/// addressed as a plain offset.
const SPREC: u64 = 0xF0;

/// `DPREC0.FORCEACTIVE` — force the device out of a low-power state.
const DPREC0_FORCEACTIVE: u32 = 1 << 3;
/// `DPREC0.RST CTL` (bits 16:14) set to `100b`, selecting halt-on-reset.
const DPREC0_HALT_ON_RESET: u32 = 0b100 << 14;
/// `DPREC0.DEBUGPOWER`.
///
/// Documented as Reserved in SLAAEO5 table 3-3, but set by both of TI's toolchain patches.
const DPREC0_DEBUGPOWER: u32 = 1 << 19;
/// `DPREC0.INHIBITSLEEP` — refuse requests to enter DEEPSLEEP.
const DPREC0_INHIBITSLEEP: u32 = 1 << 20;

/// Bits 23:21 of `DPREC0`.
///
/// Undocumented. TI's patches call these the "sticky" bits and take a recovery path when any of
/// them is set, so we do the same.
const DPREC0_STICKY: u32 = 0x00E0_0000;

/// The steady-state value TI's patches write to `DPREC0`.
const DPREC0_DEBUG_ENABLE: u32 =
    DPREC0_FORCEACTIVE | DPREC0_HALT_ON_RESET | DPREC0_DEBUGPOWER | DPREC0_INHIBITSLEEP;

/// `SPREC.SYS RST`.
const SPREC_SYS_RST: u32 = 1 << 0;

/// TX DATA register, SEC-AP register bank 0, offset 0x00.
const TXDATA: u64 = 0x00;

/// TX CONTROL register, SEC-AP register bank 0, offset 0x04.
const TXCTL: u64 = 0x04;

/// RX DATA register, SEC-AP register bank 0, offset 0x08.
const RXDATA: u64 = 0x08;

/// RX CONTROL register, SEC-AP register bank 0, offset 0x0C.
const RXCTL: u64 = 0x0C;

/// `RXVLD` bit in RXCTL.
const RXCTL_RXVLD: u32 = 1 << 0;

/// Mask of `RX CONTROL` in RXCTL.
const RXCTL_RX_CONTROL_MASK: u32 = 0x0000_00FE;

/// This value comes from CCS (mspm0_cs_dap_init.gel)
const DSSM_COMMAND_OK: u32 = 0x0100;

/// Marker struct indicating initialization sequencing for MSPM0 family parts.
#[derive(Debug)]
pub struct MSPM0 {
    /// Chip name, used to select the recovery variant.
    name: String,
    /// Whether this part needs the longer sticky-bit recovery sequence.
    long_recovery: bool,
}

impl MSPM0 {
    /// Create the sequencer for the MSPM0 family of parts.
    pub fn create(name: String) -> Arc<Self> {
        // TI ships two flavours of the recovery path. The MSPM0C110X and MSPS003FX packs use a
        // longer variant; every other family uses the short one.
        let long_recovery = name.starts_with("MSPM0C110") || name.starts_with("MSPS003F");

        Arc::new(Self {
            name,
            long_recovery,
        })
    }

    /// Read `DPREC0` and log it, mirroring the `Message()` calls in TI's debug sequences.
    fn read_dprec0(&self, interface: &mut dyn DapAccess) -> Result<u32, ArmError> {
        let pwr_ap: FullyQualifiedApAddress = ApSel::PwrAp.into();
        let value = interface.read_raw_ap_register(&pwr_ap, DPREC0)?;
        tracing::debug!("{}: DPREC0 is {:#010x}", self.name, value);
        Ok(value)
    }

    fn write_dprec0(&self, interface: &mut dyn DapAccess, value: u32) -> Result<(), ArmError> {
        let pwr_ap: FullyQualifiedApAddress = ApSel::PwrAp.into();
        interface.write_raw_ap_register(&pwr_ap, DPREC0, value)
    }

    fn write_sprec(&self, interface: &mut dyn DapAccess, value: u32) -> Result<(), ArmError> {
        let pwr_ap: FullyQualifiedApAddress = ApSel::PwrAp.into();
        interface.write_raw_ap_register(&pwr_ap, SPREC, value)
    }

    /// Recover a device whose `DPREC0` sticky bits are set.
    ///
    /// The meaning of bits 23:21 is undocumented; this reproduces what TI's packs do.
    fn recover_sticky(&self, interface: &mut dyn DapAccess) -> Result<(), ArmError> {
        tracing::warn!(
            "{}: DPREC0 sticky bits are set, running the PWR-AP recovery sequence",
            self.name
        );

        self.write_sprec(interface, SPREC_SYS_RST)?;

        if self.long_recovery {
            self.read_dprec0(interface)?;
            self.write_dprec0(interface, DPREC0_FORCEACTIVE)?;

            self.read_dprec0(interface)?;
            self.write_dprec0(interface, DPREC0_DEBUG_ENABLE | DPREC0_STICKY)?;

            self.read_dprec0(interface)?;
            self.write_sprec(interface, SPREC_SYS_RST)?;
            self.write_dprec0(interface, DPREC0_DEBUG_ENABLE)?;
        } else {
            // Writing the sticky bits back preserves them, as TI's packs do.
            self.write_dprec0(interface, DPREC0_DEBUG_ENABLE | DPREC0_STICKY)?;
        }

        self.read_dprec0(interface)?;

        Ok(())
    }
}

impl ArmDebugSequence for MSPM0 {
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        self.debug_port_start_default(interface, dp)?;

        // Everything below is specific to MSPM0: keep the device out of DEEPSLEEP for as long as
        // we are attached, otherwise the AHB-AP disappears along with power domain PD1.
        //
        // A failure here is not recoverable by us: if `DEBUGSS.SPECIAL_AUTH.PWRAPEN` is deasserted
        // in NONMAIN, a DAPBUS firewall isolates the PWR-AP entirely. Let the error propagate
        // rather than continuing into a session that will drop a few seconds later.
        let dprec0 = self.read_dprec0(interface)?;

        if dprec0 & DPREC0_STICKY == 0 {
            self.write_dprec0(interface, DPREC0_DEBUG_ENABLE)?;
        } else {
            self.recover_sticky(interface)?;
        }

        Ok(())
    }

    fn debug_core_stop(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
    ) -> Result<(), ArmError> {
        // Let the core be torn down normally first; the PWR-AP write below is what lets the device
        // sleep again, so it has to come last.
        self.debug_core_stop_default(interface, core_type)?;

        let interface = interface.get_arm_debug_interface()?;

        // Hand low-power control back to the application. Leaving INHIBITSLEEP set would keep the
        // part awake and burning current until its next reset.
        let dprec0 = self.read_dprec0(interface)?;
        self.write_dprec0(
            interface,
            dprec0 & !(DPREC0_INHIBITSLEEP | DPREC0_FORCEACTIVE),
        )?;

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // SLAAE05 generally suggests one of two methods to perform a SYSRST:
        // 1. Write SYSCTL.RESETLEVEL and SYSCTL.RESETCMD.
        // 2. Write to SPREC.RST_CTL.
        //
        // We do not do the first option because it will fault when trying to write to SYSCTL
        // and it makes the reset cause a "Software-triggered SYSRST".
        //
        // The second option always works and has a reset cause of "Debug-triggered SYSRST"
        let dap_interface = interface
            .get_arm_debug_interface()
            .expect("MSPM0 is always ARM");

        // Do not write to DPREC0.RST_CTL because ResetCatchSet will set the catch address.

        // Write SPREC.SYS_RST to do a SYSRST.
        self.write_sprec(dap_interface, SPREC_SYS_RST)?;

        // SYSRST resets both the peripherals and CPU.
        // Need to wait for the CPU to reset.
        cortex_m_wait_for_reset(interface)
    }

    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        permissions: &Permissions,
    ) -> Result<(), ArmError> {
        // Check if AHB-AP is available.
        //
        // debug_port_start makes sure that sleep is not preventing the AHB-AP from appearing.
        let ahb_ap_available = matches!(
            interface.read_raw_ap_register(default_ap, IDR::ADDRESS),
            Ok(_)
                | Err(ArmError::AccessPort {
                    source: AccessPortError::RegisterRead { .. },
                    ..
                })
        );

        // If the AHB-AP is not available it is possible that the NONMAIN region is configured to
        // prevent access to the AHB-AP. The AHB-AP would need to be unlocked with a password or
        // factory reset to clear the NONMAIN region.
        if !ahb_ap_available {
            tracing::warn!(
                "The device is locked (AHB-AP is not visible). A factory reset is required to unlock it (--allow-mass-erase)."
            );

            permissions
                .erase_all()
                .map_err(|MissingPermissions(desc)| ArmError::MissingPermissions(desc))?;

            tracing::warn!("{}: Performing SEC-AP factory reset", self.name);
            run_dssm_command(interface, DssmCommand::FactoryReset)?;
        }

        Ok(())
    }

    fn debug_erase_sequence(&self) -> Option<Arc<dyn DebugEraseSequence>> {
        Some(Arc::new(SecApEraseSequence {
            name: self.name.clone(),
        }))
    }
}

#[derive(Debug)]
pub(crate) struct SecApEraseSequence {
    name: String,
}

impl SecApEraseSequence {}

impl DebugEraseSequence for SecApEraseSequence {
    fn erase_all(&self, interface: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        tracing::warn!("{}: Performing SEC-AP mass erase", self.name);
        run_dssm_command(interface, DssmCommand::MassErase)
    }
}

fn run_dssm_command(
    interface: &mut dyn ArmDebugInterface,
    command: DssmCommand,
) -> Result<(), ArmError> {
    // Reference: Figure 4-1 from SLAAEO5

    // "Send command: TXCTL = command, TXDATA = 0"
    dssm_write_command(interface, command)?;
    dssm_write_tx_data(interface, 0)?;

    // "Force reset to run BootCode"
    //
    // SPREC can only perform a SYSRST at most. But we need a BOOTRST. Asserting the reset pin on the device
    // is the only way this can be done without accessing the AHB-AP (which may be inaccessible).
    let mut pins = Pins(0);
    pins.set_nreset(true);

    // NRST must be asserted <1s to cause a BOOTRST.
    interface.swj_pins(0, pins.0 as u32, 0)?;
    thread::sleep(Duration::from_millis(10));

    interface.swj_pins(pins.0 as u32, pins.0 as u32, 0)?;
    thread::sleep(Duration::from_millis(10));

    // "RXCTL & RXDATAAVAIL"
    //
    // try 10 times every 10ms
    for _ in 0..10 {
        // "Response OK?"
        if dssm_read_data(interface)?.is_some() {
            return Ok(());
        }

        thread::sleep(Duration::from_millis(10));
    }

    Err(ArmError::Other(format!(
        "Chip did not respond to SEC-AP command: {command:?}; the NONMAIN region may be configured to prevent mass erase or factory reset commands",
    )))
}

fn dssm_write_tx_data(interface: &mut dyn DapAccess, value: u32) -> Result<(), ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
    interface.write_raw_ap_register(&sec_ap, TXDATA, value)
}

fn dssm_write_command(interface: &mut dyn DapAccess, command: DssmCommand) -> Result<(), ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();

    // Confusingly Table 4-2 says TXVLD is bit 0 while the control word is bits 31 through 1.
    //
    // But shifting the command results in a factory reset not happening.
    // `mspm0_cs_dap_init.gel` in CCS performs no shift and just writes the command to TX_CONTROL.
    interface.write_raw_ap_register(&sec_ap, TXCTL, command as u32)
}

/// Read from the RX_DATA register if data is available.
///
/// Returns [`None`] if no data is available.
fn dssm_read_data(interface: &mut dyn DapAccess) -> Result<Option<u32>, ArmError> {
    let sec_ap: FullyQualifiedApAddress = ApSel::SecAp.into();
    let rx_control = interface.read_raw_ap_register(&sec_ap, RXCTL)?;

    if rx_control & RXCTL_RXVLD == 0 {
        return Ok(None);
    }

    // An "OK" response will set RX_CONTROL to DSSM_COMMAND_OK.
    if !((rx_control & RXCTL_RX_CONTROL_MASK) >> 1) == DSSM_COMMAND_OK {
        return Err(ArmError::Other(String::from(
            "SEC-AP failed to execute factory reset command",
        )));
    }

    let data = interface.read_raw_ap_register(&sec_ap, RXDATA)?;

    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The values TI's Keil packs and IAR macros write, pinned so a change to the bit definitions
    /// above cannot silently alter what goes on the wire.
    #[test]
    fn dprec0_values_match_ti_patches() {
        assert_eq!(DPREC0_DEBUG_ENABLE, 0x0019_0008);
        assert_eq!(DPREC0_DEBUG_ENABLE | DPREC0_STICKY, 0x00F9_0008);
        assert_eq!(DPREC0_FORCEACTIVE, 0x0000_0008);
    }

    #[test]
    fn long_recovery_is_limited_to_c110x_and_msps003fx() {
        for name in ["MSPM0C1103", "MSPM0C1104", "MSPS003F3", "MSPS003F4"] {
            assert!(
                MSPM0::create(name.to_string()).long_recovery,
                "{name} should use the long recovery sequence"
            );
        }

        for name in ["MSPM0L1306", "MSPM0L2228", "MSPM0G3507", "MSPM0G3519"] {
            assert!(
                !MSPM0::create(name.to_string()).long_recovery,
                "{name} should use the short recovery sequence"
            );
        }
    }

    /// Guards the chip-name prefix matching in `vendor/ti/mod.rs`: every built-in MSPM0 target must
    /// resolve to this sequence rather than falling through to the generic ARM default.
    #[test]
    fn all_builtin_mspm0_targets_get_the_mspm0_sequence() {
        let registry = crate::config::Registry::from_builtin_families();

        let names = [
            "MSPM0C1104",
            "MSPM0L1306",
            "MSPM0L2228",
            "MSPM0G3507",
            "MSPM0G5187",
            "MSPM0G3519",
        ];

        for name in names {
            let target = registry
                .get_target_by_name(name)
                .unwrap_or_else(|e| panic!("{name} is not a built-in target: {e}"));

            let debug_sequence = format!("{:?}", target.debug_sequence);
            assert!(
                debug_sequence.contains("MSPM0"),
                "{name} resolved to {debug_sequence}, expected the MSPM0 sequence"
            );
        }
    }
}
