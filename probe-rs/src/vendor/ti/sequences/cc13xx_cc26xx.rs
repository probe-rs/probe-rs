//! Sequences for cc13xx_cc26xx devices
use std::sync::Arc;
use std::time::Duration;

use crate::MemoryMappedRegister;
use crate::architecture::arm::armv7m::{Demcr, Dhcsr};
use crate::architecture::arm::communication_interface::DapProbe;
use crate::architecture::arm::memory::ArmMemoryInterface;
use crate::architecture::arm::sequences::{ArmDebugSequence, ArmDebugSequenceError};
use crate::architecture::arm::{ArmError, dp::DpAddress};
use crate::probe::WireProtocol;

use super::icepick::Icepick;

/// Marker struct indicating initialization sequencing for cc13xx_cc26xx family parts.
#[derive(Debug)]
pub struct CC13xxCC26xx {
    // Chip name
    name: String,
}

impl CC13xxCC26xx {
    /// Create the sequencer for the cc13xx_cc26xx family of parts.
    pub fn create(name: String) -> Arc<Self> {
        Arc::new(Self { name })
    }
}

/// Do a full system reset (emulated PIN reset)
///
/// CPU reset alone is not possible since AIRCR.SYSRESETREQ will be
/// converted to system reset on these devices.
///
/// The below code writes to the following bit
/// `AON_PMCTL.RESETCTL.SYSRESET=1`d or its equivalent based on family
async fn reset_chip(chip: &str, probe: &mut dyn ArmMemoryInterface) {
    // The CC family of device have a pattern where the 6th character of the device name dictates the family
    // Use this to determine the correct address to write to
    match chip.chars().nth(5).unwrap() {
        // Note that errors are ignored
        // writing this register will immediately trigger a system reset which causes us to lose the debug interface
        // We also don't need to worry about preserving register state because we will anyway reset.
        '0' => {
            probe.write_word_32(0x4009_0004, 0x8000_0000).await.ok();
        }
        '1' | '2' => {
            probe.write_word_32(0x4009_0028, 0x8000_0000).await.ok();
        }
        '4' => {
            probe.write_word_32(0x5809_0028, 0x8000_0000).await.ok();
        }
        _ => {
            unreachable!(
                "TI CC13xx/CC26xx debug sequence used on an unsupported chip: {chip}",
                chip = chip
            );
        }
    }
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for CC13xxCC26xx {
    async fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Check if the previous code requested a halt before reset
        let demcr = Demcr(probe.read_word_32(Demcr::get_mmio_address()).await?);

        // Do target specific reset
        reset_chip(&self.name, probe).await;

        // Since the system went down, including the debug, we should flush any pending operations
        probe.flush().await.ok();

        // Wait for the system to reset
        std::thread::sleep(Duration::from_millis(1));

        // Re-initializing the core(s) is on us.
        let ap = probe.fully_qualified_address();
        let interface = probe.get_arm_probe_interface()?;
        interface.reinitialize().await?;

        assert!(debug_base.is_none());
        self.debug_core_start(interface, &ap, core_type, None, None)
            .await?;

        if demcr.vc_corereset() {
            // TODO! Find a way to call the armv7m::halt function instead
            let mut value = Dhcsr(0);
            value.set_c_halt(true);
            value.set_c_debugen(true);
            value.enable_write();

            probe
                .write_word_32(Dhcsr::get_mmio_address(), value.into())
                .await?;
        }

        Ok(())
    }

    async fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        _dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Ensure current debug interface is in reset state.
        interface.swj_sequence(51, 0x0007_FFFF_FFFF_FFFF).await?;

        match interface.active_protocol() {
            Some(WireProtocol::Jtag) => {
                let mut icepick = Icepick::new(interface)?;
                icepick.ctag_to_jtag()?;
                icepick.select_tap(0)?;

                // Call the configure JTAG function. We don't derive the scan chain at runtime
                // for these devices, but regardless the scan chain must be told to the debug probe
                // We avoid the live scan for the following reasons:
                // 1. Only the ICEPICK is connected at boot so we need to manually the CPU to the scan chain
                // 2. Entering test logic reset disconects the CPU again
                interface.configure_jtag(true).await?;
            }
            Some(WireProtocol::Swd) => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "The cc13xx_cc26xx family doesn't support SWD".into(),
                )
                .into());
            }
            _ => {
                return Err(ArmDebugSequenceError::SequenceSpecific(
                    "Cannot detect current protocol".into(),
                )
                .into());
            }
        }

        Ok(())
    }
}
