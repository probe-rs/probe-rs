//! Sequences for TI MSPM0 devices.
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

use crate::architecture::arm::dp::DpAddress;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::ArmDebugSequence;
use crate::architecture::arm::{ArmError, DapAccess, FullyQualifiedApAddress};
use probe_rs_target::CoreType;

/// Access Port Select values used by this sequence.
#[derive(Debug, Clone, Copy)]
enum ApSel {
    /// PWR-AP: controls the power and reset state of the CPU for debug purposes.
    PwrAp = 4,
}

impl From<ApSel> for FullyQualifiedApAddress {
    fn from(apsel: ApSel) -> Self {
        FullyQualifiedApAddress::v1_with_default_dp(apsel as u8)
    }
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
