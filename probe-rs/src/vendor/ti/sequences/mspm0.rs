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
use std::time::{Duration, Instant};

use crate::MemoryMappedRegister;
use crate::architecture::arm::ap::{AddressIncrement, ApRegister, CSW, DRW, DataSize, IDR, TAR};
use crate::architecture::arm::core::armv6m::Dhcsr;
use crate::architecture::arm::dp::DpAddress;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, cortex_m_wait_for_reset};
use crate::architecture::arm::{ArmError, DapAccess, FullyQualifiedApAddress};
use probe_rs_target::CoreType;

/// Access Port Select values used by this sequence.
#[derive(Debug, Clone, Copy)]
enum ApSel {
    /// AHB-AP: memory access to the core. It lives in power domain PD1, so it is missing while
    /// the device is in a low-power state.
    AhbAp = 0,
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

/// How long an AHB-AP write is left alone before it is read back: longer than a write takes while
/// the bus runs from LFCLK.
const AHB_AP_WRITE_SETTLE: Duration = Duration::from_millis(2);

/// Whether `csw` selects the 32-bit accesses without address increment that DHCSR needs.
fn word_no_increment(csw: CSW) -> bool {
    csw.SIZE() == DataSize::U32 && csw.AddrInc() == AddressIncrement::Off
}

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

    /// Whether the AHB-AP answers.
    ///
    /// `FORCEACTIVE` needs a moment to bring PD1 back up, so this polls rather than reading once.
    /// Treating a slow wake as an absent AP would put us straight back into resetting a device
    /// that was only asleep.
    fn ahb_ap_responds(&self, interface: &mut dyn DapAccess) -> bool {
        let ahb_ap: FullyQualifiedApAddress = ApSel::AhbAp.into();

        let start = Instant::now();
        loop {
            if matches!(interface.read_raw_ap_register(&ahb_ap, IDR::ADDRESS), Ok(idr) if idr != 0)
            {
                return true;
            }
            if start.elapsed() > Duration::from_millis(10) {
                return false;
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Write an AHB-AP register on its own, give the write time to finish, and check that `done`
    /// accepts what the register reads back.
    ///
    /// While the core sleeps in STANDBY the bus runs from LFCLK, and a write the DP has accepted
    /// can be lost, without WDATAERR, when the next transfer gets WAIT at the wrong moment. So each
    /// write goes out on its own, after the one before has finished, and is read back.
    fn write_ahb_ap_checked(
        &self,
        interface: &mut dyn DapAccess,
        what: &str,
        addr: u64,
        value: u32,
        done: impl Fn(u32) -> bool,
    ) -> bool {
        let ahb_ap: FullyQualifiedApAddress = ApSel::AhbAp.into();

        if let Err(error) = interface.write_raw_ap_register(&ahb_ap, addr, value) {
            tracing::debug!("{}: writing {what} failed: {error}", self.name);
            return false;
        }
        thread::sleep(AHB_AP_WRITE_SETTLE);
        match interface.read_raw_ap_register(&ahb_ap, addr) {
            Ok(read) if done(read) => true,
            Ok(read) => {
                tracing::debug!("{}: {what} did not take, read back {read:#010x}", self.name);
                false
            }
            Err(error) => {
                tracing::debug!("{}: reading {what} back failed: {error}", self.name);
                false
            }
        }
    }

    /// Take a core that sleeps in STANDBY out of `WFI` with a halt, then resume it.
    ///
    /// `FORCEACTIVE` makes the AHB-AP answer but leaves the core in `WFI` and the bus slow. Once
    /// the core has left `WFI`, `INHIBITSLEEP` keeps it out of STANDBY for the rest of the session.
    /// A debug halt is a wakeup the application has to tolerate (ARMv6-M B1.5.17). Returns false
    /// if an access failed, or if a sleeping core could not be woken.
    fn wake_core(&self, interface: &mut dyn DapAccess) -> bool {
        let ahb_ap: FullyQualifiedApAddress = ApSel::AhbAp.into();

        let mut csw = match interface.read_raw_ap_register(&ahb_ap, CSW::ADDRESS) {
            Ok(csw) => CSW::from_raw(csw),
            Err(error) => {
                tracing::debug!("{}: reading the AHB-AP CSW failed: {error}", self.name);
                return false;
            }
        };
        if !word_no_increment(csw) {
            csw.set_SIZE(DataSize::U32);
            csw.set_AddrInc(AddressIncrement::Off);
            if !self.write_ahb_ap_checked(interface, "CSW", CSW::ADDRESS, csw.into(), |read| {
                word_no_increment(CSW::from_raw(read))
            }) {
                return false;
            }
        }

        let dhcsr_address = Dhcsr::get_mmio_address() as u32;
        if !self.write_ahb_ap_checked(interface, "TAR", TAR::ADDRESS, dhcsr_address, |read| {
            read == dhcsr_address
        }) {
            return false;
        }

        let dhcsr = match interface.read_raw_ap_register(&ahb_ap, DRW::ADDRESS) {
            Ok(dhcsr) => Dhcsr(dhcsr),
            Err(error) => {
                tracing::debug!("{}: reading DHCSR failed: {error}", self.name);
                return false;
            }
        };
        if !dhcsr.s_sleep() {
            return true;
        }

        let mut halt = Dhcsr(0);
        halt.set_c_debugen(true);
        halt.set_c_halt(true);
        halt.enable_write();
        if !self.write_ahb_ap_checked(
            interface,
            "the halt request",
            DRW::ADDRESS,
            halt.into(),
            |read| Dhcsr(read).s_halt(),
        ) {
            return false;
        }

        let mut resume = Dhcsr(0);
        resume.set_c_debugen(true);
        resume.enable_write();
        self.write_ahb_ap_checked(
            interface,
            "the resume request",
            DRW::ADDRESS,
            resume.into(),
            |read| !Dhcsr(read).s_halt(),
        )
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
            return Ok(());
        }

        // The sticky bits are set on any device that has been in a low-power state, not only on
        // one that needs recovering, so finding them set is not on its own a reason to reset the
        // device. Write what the recovery would write and see whether the AHB-AP comes back:
        // `FORCEACTIVE` is what makes it discoverable again (SLAAEO5 section 3.1). Only when it
        // stays missing, or the core cannot be woken, is there something wrong, and only then is a
        // system reset worth its cost.
        self.write_dprec0(interface, DPREC0_DEBUG_ENABLE | DPREC0_STICKY)?;

        if self.ahb_ap_responds(interface) && self.wake_core(interface) {
            return Ok(());
        }

        self.recover_sticky(interface)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::architecture::arm::dp::{Ctrl, DPIDR, DpRegister, DpRegisterAddress};

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

    /// An MSPM0 whose `DPREC0` sticky bits are set, as after any low-power entry. Its AHB-AP
    /// answers once `FORCEACTIVE` is set if `ap_returns`. Its core sleeps in `WFI` if `asleep`, and
    /// its halt requests are lost if `halt_lost`, as a write the next transfer's WAIT catches can
    /// be while the bus is slow.
    #[derive(Default)]
    struct SleepingMspm0 {
        ap_returns: bool,
        asleep: bool,
        halt_lost: bool,
        dprec0: u32,
        sprec_writes: Vec<u32>,
        csw: u32,
        tar: u32,
        halted: bool,
        halts: usize,
    }

    impl SleepingMspm0 {
        /// Whether a DRW access reaches DHCSR: TAR points at it and CSW selects word accesses.
        fn dhcsr_selected(&self) -> bool {
            u64::from(self.tar) == Dhcsr::get_mmio_address() && self.csw & 0b111 == 0b010
        }
    }

    impl DapAccess for SleepingMspm0 {
        fn read_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            addr: DpRegisterAddress,
        ) -> Result<u32, ArmError> {
            // A DPv2 SW-DP that is already powered up.
            Ok(match addr {
                DPIDR::ADDRESS => 0x6BA0_2477,
                Ctrl::ADDRESS => 0xF000_0000,
                _ => 0,
            })
        }

        fn write_raw_dp_register(
            &mut self,
            _dp: DpAddress,
            _addr: DpRegisterAddress,
            _value: u32,
        ) -> Result<(), ArmError> {
            Ok(())
        }

        fn read_raw_ap_register(
            &mut self,
            ap: &FullyQualifiedApAddress,
            addr: u64,
        ) -> Result<u32, ArmError> {
            Ok(match (ap.ap_v1()?, addr) {
                (4, DPREC0) => self.dprec0,
                (0, IDR::ADDRESS) if self.ap_returns && self.dprec0 & DPREC0_FORCEACTIVE != 0 => {
                    0x0477_0031
                }
                (0, CSW::ADDRESS) => self.csw,
                (0, TAR::ADDRESS) => self.tar,
                // S_HALT is bit 17, S_SLEEP bit 18.
                (0, DRW::ADDRESS) if self.dhcsr_selected() => {
                    (u32::from(self.halted) << 17) | (u32::from(self.asleep && !self.halted) << 18)
                }
                _ => 0,
            })
        }

        fn write_raw_ap_register(
            &mut self,
            ap: &FullyQualifiedApAddress,
            addr: u64,
            value: u32,
        ) -> Result<(), ArmError> {
            match (ap.ap_v1()?, addr) {
                (4, DPREC0) => self.dprec0 = value,
                (4, SPREC) => self.sprec_writes.push(value),
                (0, CSW::ADDRESS) => self.csw = value,
                (0, TAR::ADDRESS) => self.tar = value,
                (0, DRW::ADDRESS) if self.dhcsr_selected() && value >> 16 == 0xA05F => {
                    if !Dhcsr(value).c_halt() {
                        self.halted = false;
                    } else if !self.halt_lost {
                        self.halted = true;
                        self.halts += 1;
                    }
                }
                _ => {}
            }
            Ok(())
        }
    }

    #[test]
    fn sticky_bits_reset_the_part_only_if_it_cannot_be_woken() {
        // (AHB-AP comes back, core asleep, halt lost) -> (reset, halts that landed)
        for (ap_returns, asleep, halt_lost, resets, halts) in [
            (true, true, false, false, 1),
            (true, true, true, true, 0),
            (true, false, false, false, 0),
            (false, true, false, true, 0),
        ] {
            let sequence = MSPM0::create("MSPM0L1306".to_string());
            let mut part = SleepingMspm0 {
                ap_returns,
                asleep,
                halt_lost,
                dprec0: DPREC0_STICKY,
                ..SleepingMspm0::default()
            };

            sequence
                .debug_port_start(&mut part, DpAddress::Default)
                .expect("debug_port_start should succeed");

            let case = format!("ap_returns {ap_returns}, asleep {asleep}, halt_lost {halt_lost}");
            assert_eq!(
                part.sprec_writes.contains(&SPREC_SYS_RST),
                resets,
                "{case}: SPREC writes {:x?}",
                part.sprec_writes
            );
            assert_eq!(part.halts, halts, "{case}: halts that landed");
            assert!(!part.halted, "{case}: the core was left halted");
        }
    }
}
