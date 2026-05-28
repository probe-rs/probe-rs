//! Implement chip-specific dual-core reset for RP235x

use crate::MemoryMappedRegister;
use crate::architecture::arm::armv6m::Demcr;
use crate::architecture::arm::dp::{Ctrl, DpAddress, DpRegister};
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, cortex_m_reset_system};
use crate::architecture::arm::{ApV2Address, ArmError, FullyQualifiedApAddress};
use std::sync::Arc;
use std::time::Duration;

const SIO_CPUID_OFFSET: u64 = 0xd000_0000;
const RP_AP: FullyQualifiedApAddress =
    FullyQualifiedApAddress::v2_with_dp(DpAddress::Default, ApV2Address(Some(0x80000)));

/// An address in RAM, used for validating that the core is working.
const RAM_ADDRESS: u64 = 0x2000_0000;

/// Speed to use during the rescue reset sequence. The RP2350's debug
/// subsystem needs time to reinitialize after RESCUE_RESTART; running
/// at a low speed naturally spaces out the transactions enough that we
/// don't race the chip's internal reset sequencer.
const RESET_SPEED_KHZ: u32 = 1_000;

/// To ensure we enter the BOOTSEL mode, we need to write specific values to
/// the watchdog SCRATCH registers. These are all defined as offsets from
/// this value.
const WATCHDOG_BASE: u64 = 0x400d_8000;

/// This magic value indicates the special watchdog control flow is active.
const WATCHDOG_SCRATCH4: u64 = WATCHDOG_BASE + 0x1c;

/// This magic value indicates the special watchdog control flow is active.
const WATCHDOG_SCRATCH5: u64 = WATCHDOG_BASE + 0x20;

/// This is the stack pointer when the special watchdog control flow is active.
const WATCHDOG_SCRATCH6: u64 = WATCHDOG_BASE + 0x24;

/// This is the entrypoint when the special watchdog control flow is active.
const WATCHDOG_SCRATCH7: u64 = WATCHDOG_BASE + 0x28;

/// This magic value must be written to SCRATCH4.
const WATCHDOG_MAGIC: u32 = 0xb007_c0d3;

/// This magic value must be written to SCRATCH5 and XORed with the
/// contents of SCRATCH7. The value is equal to -WATCHDOG_MAGIC.
const WATCHDOG_MAGIC_NEGATED: u32 = 0u32.wrapping_sub(WATCHDOG_MAGIC);

/// When the stack is set to this value, the device enters BOOTSEL. This must
/// be written to SCRATCH6.
const WATCHDOG_MAGIC_BOOTSEL: u32 = 2;

/// When this entrypoint is selected, the stack pointer is used as
/// the boot type. This should be written to SCRATCH7.
const WATCHDOG_MAGIC_ENTRY: u32 = 0xb007_c0d3;

/// The bootrom appears to take up to 30 milliseconds, based on observations.
const BOOTROM_STARTUP_TIME: Duration = Duration::from_millis(30);

/// Debug implementation for RP235x
#[derive(Debug)]
pub struct Rp235x {}

impl Rp235x {
    /// Create a debug sequencer for a Raspberry Pi RP235x
    pub fn create() -> Arc<Self> {
        Arc::new(Rp235x {})
    }
}

impl ArmDebugSequence for Rp235x {
    fn reset_system(
        &self,
        core: &mut dyn ArmMemoryInterface,
        core_type: crate::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("reset_system(interface, {core_type:?}, {debug_base:x?})");
        // Only perform a system reset from core 0
        let core_id = core.read_word_32(SIO_CPUID_OFFSET)?;
        if core_id != 0 {
            tracing::warn!("Skipping reset of core {core_id}");
            return Ok(());
        }

        // Since we're resetting the core, the catch_reset flag will get lost.
        // Note whether we should re-set it after entering Rescue Mode.
        let should_catch_reset =
            Demcr(core.read_word_32(Demcr::get_mmio_address())?).vc_corereset();

        let ap = core.fully_qualified_address();
        let arm_interface = core.get_arm_debug_interface()?;

        // Take note of the existing values for CTRL. We will need to restore these after entering
        // rescue mode.
        let existing_core_0 = arm_interface.read_raw_dp_register(ap.dp(), Ctrl::ADDRESS)?;
        tracing::trace!("Core 0 DP_CTRL: {existing_core_0:08x}");

        // Drop to a safe speed for the rescue reset sequence. At high speeds
        // the AP transactions after RESCUE_RESTART arrive before the RP2350's
        // debug subsystem has reinitialized, causing FAULT on DRW. Running at
        // a low speed naturally spaces transactions far enough apart that the
        // chip always has time to recover.
        let probe_speed = arm_interface.try_dap_probe_mut().map(|probe| {
            let speed = probe.speed_khz();
            tracing::debug!("Lowering SWD speed to {RESET_SPEED_KHZ} kHz for reset sequence");
            let _ = probe.set_speed(RESET_SPEED_KHZ);
            speed
        });

        // Put the SoC in rescue reset as the datasheet.
        //
        // This process consists of setting a flag in the CTRL register of
        // the RP-AP, which causes a flag to be set in POWMAN CHIP_RESET
        // (RESCUE) and the SoC to reset. The BootROM checks for the RESCUE
        // flag, and if set, halts the system. We then attach as usual.
        //
        // See: 'RP2350 Datasheet', sections:
        //   3.5.8: Rescue Reset
        //   3.5.10.1: RP-AP list of registers

        // CTRL register offset within RP-AP.
        const CTRL: u64 = 0;
        const RESCUE_RESTART: u32 = 0x8000_0000;

        // Poke 1 to CTRL.RESCUE_RESTART.
        let ctrl = arm_interface.read_raw_ap_register(&RP_AP, CTRL)?;
        arm_interface.write_raw_ap_register(&RP_AP, CTRL, ctrl | RESCUE_RESTART)?;
        // Poke 0 to CTRL.RESCUE_RESTART.
        let ctrl = arm_interface.read_raw_ap_register(&RP_AP, CTRL)?;
        arm_interface.write_raw_ap_register(&RP_AP, CTRL, ctrl & !RESCUE_RESTART)?;

        let new_core_0 = arm_interface.read_raw_dp_register(ap.dp(), Ctrl::ADDRESS)?;
        tracing::trace!("new core 0 CTRL: {new_core_0:08x}");

        // Start the debug core back up which brings it out of Rescue Mode.
        self.debug_core_start(arm_interface, &ap, core_type, debug_base, None)?;

        // Restore the ctrl values
        tracing::trace!("Restoring ctrl values");
        arm_interface.write_raw_dp_register(ap.dp(), Ctrl::ADDRESS, existing_core_0)?;

        // By default, clocks coming out of reset are not fast. This results in programming
        // speed that's slower than it otherwise should be. Run the boot ROM, which configures
        // the PLL and increases flash clock speed.
        core.write_word_32(WATCHDOG_SCRATCH4, WATCHDOG_MAGIC)?;
        core.write_word_32(
            WATCHDOG_SCRATCH5,
            WATCHDOG_MAGIC_ENTRY ^ WATCHDOG_MAGIC_NEGATED,
        )?;
        core.write_word_32(WATCHDOG_SCRATCH6, WATCHDOG_MAGIC_BOOTSEL)?;
        core.write_word_32(WATCHDOG_SCRATCH7, WATCHDOG_MAGIC_ENTRY)?;

        // Perform a reset to get out of Rescue Mode and into BOOTSEL mode.
        cortex_m_reset_system(core)?;

        // Wait for the bootrom to run.
        std::thread::sleep(BOOTROM_STARTUP_TIME);

        // If we were set to catch the reset before, set it up again.
        if should_catch_reset {
            tracing::trace!("Re-setting reset catch");
            self.reset_catch_set(core, core_type, debug_base)?
        }

        // Perform a reset to get out of BOOTSEL mode by poking AIRCR.
        cortex_m_reset_system(core)?;

        // Restore speed before handing back to core-level operations.
        let arm_interface = core.get_arm_debug_interface()?;
        if let (Some(probe), Some(speed)) = (arm_interface.try_dap_probe_mut(), probe_speed) {
            tracing::debug!("Restoring SWD speed to {speed} kHz");
            let _ = probe.set_speed(speed);
        }

        // As a final check, make sure we can read from RAM.
        core.read_word_32(RAM_ADDRESS).map(|_| ())
    }
}
