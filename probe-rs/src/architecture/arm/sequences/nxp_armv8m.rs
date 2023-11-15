//! Sequences for NXP chips that use ARMv8-M cores.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ap::{AccessPort, MemoryAp},
        communication_interface::Initialized,
        core::armv8m::{Aircr, Demcr, Dhcsr},
        memory::adi_v5_memory_interface::ArmProbe,
        ApAddress, ArmCommunicationInterface, ArmError, DapAccess, DpAddress, Pins,
    },
    core::MemoryMappedRegister,
};

use super::ArmDebugSequence;

/// Debug sequences for MIMXRT5xxS MCUs.
///
/// MCUs in this series do not have any on-board flash memory, and instead
/// there is a non-programmable boot ROM which attempts to find a suitable
/// program from a variety of different sources. The entry point for
/// application code therefore varies depending on the boot medium.
///
/// **Note:** These sequences assume that the chip's `PIO4_5` is connected
/// to an active-low reset signal on the NOR flash chip, and will attempt
/// to reset the flash when resetting the overall system. This pin selection
/// matches the evaluation kit (MIMXRT595-EVK) but there's currently no way
/// to make that customizable for other boards.
///
/// Because the system begins execution in the boot ROM, it isn't possible
/// to use a standard reset vector catch on this platform. Instead, the series
/// datasheet (section 60.3.4) describes the following protocol:
///
/// - Set a data watchpoint for a read from location 0x50002034.
/// - Use SYSRESETREQ to reset the core and peripherals.
/// - Wait 100ms to allow the boot ROM to re-enable debug.
/// - Check whether the core is halted due to the watchpoint, by checking DHCSR.
/// - If the core doesn't halt or halts for some reason other than the
///   watchpoint, use the special debug mailbox protocol to exit the ISP mode
///   and enter an infinite loop, at which point we can halt the MCU explicitly.
/// - Clear the data watchpoint.
///
/// The debug mailbox protocol handles, among other things, recovering debug
/// access when the part enters its ISP mode. ISP mode has debug disabled to
/// prevent tampering with the system's security features. Datasheet
/// section 60.3.1 describes the special debug recovery process.
//
// This type's [`ArmDebugSequence`] implementation is based on the sequences
// defined in the CMSIS Pack for MIMXRT595S, but should be compatible with
// all parts in this series. The implementation closely follows the CMSIS Pack
// structure and its comments for ease of comparison.
#[derive(Debug)]
pub struct MIMXRT5xxS {}

impl MIMXRT5xxS {
    const DWT_COMP0: u64 = 0xE0001020;
    const DWT_FUNCTION0: u64 = 0xE0001028;
    const SYSTEM_STICK_CALIB_ADDR: u32 = 0x50002034;
    const FLEXSPI_NOR_FLASH_HEADER_ADDR: u64 = 0x08000400;
    const FLEXSPI_NOR_FLASH_HEADER_MAGIC: u32 = 0x42464346;

    /// Create a sequence handle for the MIMXRT5xxS.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {})
    }

    /// Runtime validation of core type.
    fn check_core_type(&self, core_type: crate::CoreType) -> Result<(), ArmError> {
        if core_type != crate::CoreType::Armv8m {
            // Caller has selected the wrong chip name, presumably.
            return Err(ArmError::ArchitectureRequired(&["ARMv8"]));
        }
        Ok(())
    }

    /// A port of the "WaitForStopAfterReset" sequence from the CMSIS Pack for
    /// this chip.
    fn wait_for_stop_after_reset(&self, probe: &mut dyn ArmProbe) -> Result<(), ArmError> {
        tracing::trace!("waiting for MIMXRT5xxS halt after reset");

        // Note: despite the name of this sequence in the CMSIS Pack, the
        // given implementation doesn't actually _wait_ for stop, and instead
        // just forces stopping itself. This is because there is no reliable
        // way to detect reset that works in all cases: the boot ROM might
        // jump into ISP mode, in which case we need to use the debug mailbox
        // to regain debug control.

        // Give bootloader time to do what it needs to do
        std::thread::sleep(Duration::from_micros(100_000));

        let ap: MemoryAp = probe.ap();
        let dp = ap.ap_address().dp;
        let start = Instant::now();
        while !self.csw_debug_ready(probe.get_arm_communication_interface()?, ap)?
            && start.elapsed() < Duration::from_micros(300_000)
        {
            // Wait for either condition
        }
        let enabled_mailbox =
            self.enable_debug_mailbox(probe.get_arm_communication_interface()?, dp, ap)?;

        // Halt the core in case it didn't stop at a breakpiont.
        tracing::trace!("halting MIMXRT5xxS Cortex-M33 core");
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        probe.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        probe.flush()?;

        if enabled_mailbox {
            // We'll double-check now to make sure we're in a reasonable state.
            if !self.csw_debug_ready(probe.get_arm_communication_interface()?, ap)? {
                tracing::warn!("MIMXRT5xxS is still not ready to debug, even after using DebugMailbox to activate session");
            }
        }

        // Clear watch point
        probe.write_word_32(Self::DWT_COMP0, 0x0)?;
        probe.write_word_32(Self::DWT_FUNCTION0, 0x0)?;
        probe.flush()?;
        tracing::trace!("cleared data watchpoint for MIMXRT5xxS reset");

        // As a heuristic for whether startup seems to have succeeded, we'll
        // probe the location where the SPI Flash configuration block would
        // be and see if it starts with the expected magic number.
        // This is just a logged warning rather than an error (as long as we
        // manage to read _something_) because the user might not actually be
        // intending to use the FlexSPI0 flash device for boot.
        let probed = probe.read_word_32(Self::FLEXSPI_NOR_FLASH_HEADER_ADDR)?;
        if probed != Self::FLEXSPI_NOR_FLASH_HEADER_MAGIC {
            tracing::warn!(
                "FlexSPI0 NOR flash config block starts with {:#010x} (valid blocks start with {:#010x})",
                probed, Self::FLEXSPI_NOR_FLASH_HEADER_MAGIC,
            );
        } else {
            tracing::trace!(
                "FlexSPI0 NOR flash config block starts with {:#010x}, as expected",
                probed
            );
        }

        Ok(())
    }

    fn reset_flash(&self, interface: &mut dyn ArmProbe) -> Result<(), ArmError> {
        tracing::trace!("MIMXRT595S-EVK FlexSPI flash reset (pulse PIO4_5)");

        // FIXME: We do this by twiddling PIO4_5, which is where the flash
        // reset pin is connected on MIMX595-EVK, but this code should not
        // make any assumptions about the evaluation board; how can we
        // generalize this so that the reset is configurable?
        interface.write_word_32(0x40001044, 1 << 24)?; // enable GPIO clock
        interface.write_word_32(0x40000074, 1 << 24)?; // take GPIO out of reset
        interface.write_word_32(0x40004214, 0x130)?; // full drive and pullup
        interface.write_word_32(0x40102010, 1 << 5)?; // PIO4_5 is an output
        interface.write_word_32(0x40103214, 0)?; // PIO4_5 is driven low
        std::thread::sleep(Duration::from_millis(100));

        interface.write_word_32(0x40102010, 0)?; // PIO4_5 is an input
        interface.flush()?;
        std::thread::sleep(Duration::from_millis(100));

        Ok(())
    }

    fn csw_debug_ready(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        ap: MemoryAp,
    ) -> Result<bool, ArmError> {
        let csw = interface.read_raw_ap_register(ap.ap_address(), 0x00)?;

        Ok(csw & 0x40 != 0)
    }

    /// A port of the "EnableDebugMailbox" sequence from the CMSIS Pack for
    /// this chip.
    ///
    /// Returns true if the debug mailbox was successfully enabled, or
    /// false if enabling the debug mailbox isn't necessary. Returns an error
    /// if it was necessary but unsuccessful.
    fn enable_debug_mailbox(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
        mem_ap: MemoryAp,
    ) -> Result<bool, ArmError> {
        // Check AHB-AP CSW DbgStatus to decide if need enable DebugMailbox
        if self.csw_debug_ready(interface, mem_ap)? {
            tracing::trace!("don't need to enable MIMXRT5xxS DebugMailbox");
            return Ok(false);
        }

        tracing::debug!("enabling MIMXRT5xxS DebugMailbox");

        let ap_addr = ApAddress { dp, ap: 2 };

        // CMSIS Pack implementation reads APIDR and DPIDR and passes each
        // to the "Message" function, but otherwise does nothing with those
        // results, so we skip that here.

        // Active DebugMailbox
        interface.write_raw_ap_register(ap_addr, 0x0, 0x00000021)?;
        std::thread::sleep(Duration::from_micros(30_000));
        interface.read_raw_ap_register(ap_addr, 0x0)?;

        // Enter Debug Session
        interface.write_raw_ap_register(ap_addr, 0x4, 0x00000007)?;
        std::thread::sleep(Duration::from_micros(30_000));
        interface.read_raw_ap_register(ap_addr, 0x0)?;

        tracing::debug!("entered MIMXRT5xxS debug session");

        Ok(true)
    }
}

impl ArmDebugSequence for MIMXRT5xxS {
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        const SW_DP_ABORT: u8 = 0x0;
        const DP_CTRL_STAT: u8 = 0x4;
        const DP_SELECT: u8 = 0x8;

        tracing::trace!("MIMXRT5xxS debug port start");

        // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits of CTRL/STAT Register by write to ABORT register
        interface.write_raw_dp_register(dp, SW_DP_ABORT, 0x0000001E)?;

        // Switch to DP Register Bank 0
        interface.write_raw_dp_register(dp, DP_SELECT, 0x00000000)?;

        // Read DP CTRL/STAT Register and check if CSYSPWRUPACK and CDBGPWRUPACK bits are set
        let powered_down =
            (interface.read_raw_dp_register(dp, DP_CTRL_STAT)? & 0xA0000000) != 0xA0000000;
        if powered_down {
            tracing::trace!("MIMXRT5xxS is powered down, so requesting power-up");

            // Request Debug/System Power-Up
            interface.write_raw_dp_register(dp, DP_CTRL_STAT, 0x50000000)?;

            // Wait for Power-Up Request to be acknowledged
            let start = Instant::now();
            while (interface.read_raw_dp_register(dp, DP_CTRL_STAT)? & 0xA0000000) != 0xA0000000 {
                if start.elapsed() >= Duration::from_micros(1_000_000) {
                    return Err(ArmError::Timeout);
                }
            }
        } else {
            tracing::trace!("MIMXRT5xxS debug port is already powered");
        }

        // SWD Specific Part of sequence
        // TODO: Should we skip this if we're not using SWD? How?
        // CMSIS Pack code uses: <control if="(__protocol &amp; 0xFFFF) == 2">
        {
            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            interface.write_raw_dp_register(dp, DP_CTRL_STAT, 0x50000F00)?;

            // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits of CTRL/STAT Register by write to ABORT register
            interface.write_raw_dp_register(dp, SW_DP_ABORT, 0x0000001E)?;

            let ap = ApAddress { dp, ap: 0 };
            let mem_ap = MemoryAp::new(ap);
            self.enable_debug_mailbox(interface, dp, mem_ap)?;
        }

        tracing::trace!("MIMXRT5xxS debug port start was successful");

        Ok(())
    }

    fn reset_system(
        &self,
        probe: &mut dyn ArmProbe,
        core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        self.check_core_type(core_type)?;

        tracing::trace!("MIMXRT5xxS reset system");

        // Halt the core
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();
        probe.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        probe.flush()?;

        // Clear VECTOR CATCH and set TRCENA
        let mut demcr: Demcr = probe.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_trcena(true);
        demcr.set_vc_corereset(false);
        probe.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        probe.flush()?;

        // Reset the flash peripheral on FlexSPI0, if any.
        self.reset_flash(probe)?;

        // Set watch point at SYSTEM_STICK_CALIB access
        probe.write_word_32(Self::DWT_COMP0, Self::SYSTEM_STICK_CALIB_ADDR)?;
        probe.write_word_32(Self::DWT_FUNCTION0, 0x00000814)?;
        probe.flush()?;
        tracing::trace!("set data watchpoint for MIMXRT5xxS reset");

        // Execute SYSRESETREQ via AIRCR
        let mut aircr = Aircr(0);
        aircr.set_sysresetreq(true);
        aircr.vectkey();
        // (we need to ignore errors here because the reset will make this
        // operation seem to have failed.)
        probe
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();
        probe.flush().ok();

        tracing::trace!("MIMXRT5xxS reset system was successful; waiting for halt after reset");

        self.wait_for_stop_after_reset(probe)
    }

    fn reset_hardware_deassert(&self, memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
        tracing::trace!("MIMXRT5xxS reset hardware deassert");
        let n_reset = Pins(0x80).0 as u32;

        let can_read_pins = memory.swj_pins(0, n_reset, 0)? != 0xffff_ffff;

        thread::sleep(Duration::from_millis(50));

        let mut assert_n_reset = || memory.swj_pins(n_reset, n_reset, 0);

        if can_read_pins {
            let start = Instant::now();
            let timeout_occured = || start.elapsed() > Duration::from_secs(1);

            while assert_n_reset()? & n_reset == 0 || !timeout_occured() {
                // Block until either condition passes
            }
        } else {
            assert_n_reset()?;
            thread::sleep(Duration::from_micros(100000));
        }

        Ok(())
    }

    // "ResetHardware" intentionally omitted because the default implementation
    // seems equivalent to the one in the CMSIS-Pack.
}
