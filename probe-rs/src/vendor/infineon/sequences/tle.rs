//! Debug sequences for Infineon MOTIX™ TLE98xx / TLE99xx motor-control MCUs.
//!
//! These devices gate the SWD debug interface behind their BootROM. After every
//! cold or warm reset the BootROM samples the TMS (SWDIO) and SWDCLK pins: only
//! if both are read high does it continue start-up in *debug mode* and enable
//! the debug interface. Otherwise it boots into user mode with debug disabled,
//! and a debugger cannot connect.
//!
//! To make the device reliably enter debug mode we drive both pins high and let
//! the BootROM's "TMS device reset" mechanism trigger a reset (holding TMS and
//! SWDCLK high for more than 200 MCLK cycles triggers a device reset). With the
//! pins still held high while the BootROM executes, the device latches into
//! debug mode and the regular SWD connection sequence can proceed.
//!
//! References (sections "Debug mode" / "Debug mode entry" /
//! "Device reset during debug mode"):
//! - MOTIX™ TLE987x user manual, "Booting scheme"
//! - MOTIX™ TLE988x/TLE989x user manual

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::architecture::arm::{
    ArmError, Pins,
    communication_interface::DapProbe,
    dp::DpAddress,
    sequences::{ArmDebugSequence, DefaultArmSequence},
};

/// Debug sequence for Infineon MOTIX™ TLE98xx / TLE99xx MCUs.
#[derive(Debug)]
pub struct InfineonTle;

impl InfineonTle {
    /// Create the debug sequencer for an Infineon TLE motor-control MCU.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self)
    }

    /// Reset the device while holding TMS (SWDIO) and SWDCLK high so that the
    /// BootROM latches the device into debug mode.
    ///
    /// The device only starts up in debug mode if the BootROM samples both TMS
    /// and SWDCLK high right after a reset. Holding the pins high on their own
    /// is not enough: the "TMS device reset" mechanism is disabled until the
    /// next cold/warm reset, so we have to trigger a fresh reset ourselves.
    /// We do that with the nRESET line (wired to the device reset pin on the
    /// debug connector) while keeping TMS and SWDCLK high across the reset and
    /// the subsequent BootROM execution.
    fn enter_debug_mode(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // The pins we drive: nRESET, TMS (SWDIO) and SWDCLK.
        let mut select = Pins(0);
        select.set_nreset(true);
        select.set_swdio_tms(true);
        select.set_swclk_tck(true);
        let select = select.0 as u32;

        // TMS and SWDCLK high, nRESET asserted (driven low).
        let mut reset_asserted = Pins(0);
        reset_asserted.set_swdio_tms(true);
        reset_asserted.set_swclk_tck(true);
        let reset_asserted = reset_asserted.0 as u32;

        // TMS and SWDCLK high, nRESET released (driven high).
        let mut reset_released = Pins(0);
        reset_released.set_swdio_tms(true);
        reset_released.set_swclk_tck(true);
        reset_released.set_nreset(true);
        let reset_released = reset_released.0 as u32;

        tracing::debug!("Infineon TLE: resetting with TMS/SWDCLK high to enter debug mode");

        // Assert nRESET while holding TMS and SWDCLK high.
        let _ = interface.swj_pins(reset_asserted, select, 0)?;
        thread::sleep(Duration::from_millis(10));

        // Release nRESET while keeping TMS and SWDCLK high, so that the BootROM
        // samples them high after the reset and starts up in debug mode instead
        // of user mode. The hold time has to cover oscillator settling and
        // BootROM execution; a few milliseconds is ample.
        let _ = interface.swj_pins(reset_released, select, 0)?;
        thread::sleep(Duration::from_millis(10));

        Ok(())
    }
}

impl ArmDebugSequence for InfineonTle {
    fn debug_port_setup(
        &self,
        interface: &mut dyn DapProbe,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        // Make the BootROM bring the device up in debug mode before attempting
        // the regular SWD connection sequence.
        self.enter_debug_mode(interface)?;

        // Run the standard debug port setup (SWJ switch + line reset + connect).
        DefaultArmSequence(()).debug_port_setup(interface, dp)
    }

    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // A device reset has to keep TMS and SWDCLK high so that the BootROM
        // re-enters (preserves) debug mode after the reset, per the "Device
        // reset during debug mode" procedure in the user manuals.
        let mut select = Pins(0);
        select.set_nreset(true);
        select.set_swdio_tms(true);
        select.set_swclk_tck(true);

        // nRESET low (asserted) while TMS and SWDCLK are held high.
        let mut output = Pins(0);
        output.set_swdio_tms(true);
        output.set_swclk_tck(true);

        let _ = interface.swj_pins(output.0 as u32, select.0 as u32, 0)?;

        Ok(())
    }
}
