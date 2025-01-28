//! Sequences for NXP chips that use ARMv8-M cores.

use bitfield::bitfield;
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ap_v1::{AccessPortError, AccessPortType, GenericAp, Register, IDR},
        communication_interface::Initialized,
        core::armv8m::{Aircr, Demcr, Dhcsr},
        dp::{Abort, Ctrl, DpAccess, DpAddress, DpRegister, SelectV1, DPIDR},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
        ArmCommunicationInterface, ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress,
        Pins,
    },
    core::MemoryMappedRegister,
};

/// OL23D0 debug sequences.
pub mod ol23d0;

/// Start the debug port, and return if the device was (true) or wasn't (false)
/// powered down.
///
/// Note that this routine only supports SWD protocols. See the inline TODOs to
/// understand where JTAG support should go.
fn debug_port_start(
    interface: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
    select: SelectV1,
) -> Result<bool, ArmError> {
    interface.write_dp_register(dp, select)?;

    let ctrl = interface.read_dp_register::<Ctrl>(dp)?;

    let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());

    if powered_down {
        let mut ctrl = Ctrl(0);
        ctrl.set_cdbgpwrupreq(true);
        ctrl.set_csyspwrupreq(true);

        interface.write_dp_register(dp, ctrl)?;

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

        // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
        let mut ctrl = Ctrl(0);

        ctrl.set_cdbgpwrupreq(true);
        ctrl.set_csyspwrupreq(true);

        ctrl.set_mask_lane(0b1111);

        interface.write_dp_register(dp, ctrl)?;

        let mut abort = Abort(0);

        abort.set_orunerrclr(true);
        abort.set_wderrclr(true);
        abort.set_stkerrclr(true);
        abort.set_stkcmpclr(true);

        interface.write_dp_register(dp, abort)?;
    }

    Ok(powered_down)
}

/// The sequence handle for the LPC55Sxx family.
#[derive(Debug)]
pub struct LPC55Sxx(());

impl LPC55Sxx {
    /// Create a sequence handle for the LPC55Sxx.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for LPC55Sxx {
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("debug_port_start");

        let _powered_down = self::debug_port_start(interface, dp, SelectV1(0))?;

        // Per 51.6.2 and 51.6.3 there is no need to issue a debug mailbox
        // command if we're attaching to a valid target. In fact, running
        // the debug mailbox _prevents_ this from attaching to a running
        // target since the debug mailbox is a separate code path.
        // if _powered_down {
        //     enable_debug_mailbox(interface, dp)?;
        // }

        Ok(())
    }

    fn reset_catch_set(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut reset_vector: u32 = 0xffff_ffff;
        let mut reset_vector_addr = 0x0000_0004;
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        // Write some stuff
        interface.write_word_32(0x40034010, 0x00000000)?; // Program Flash Word Start Address to 0x0 to read reset vector (STARTA)
        interface.write_word_32(0x40034014, 0x00000000)?; // Program Flash Word Stop Address to 0x0 to read reset vector (STOPA)
        interface.write_word_32(0x40034080, 0x00000000)?; // DATAW0: Prepare for read
        interface.write_word_32(0x40034084, 0x00000000)?; // DATAW1: Prepare for read
        interface.write_word_32(0x40034088, 0x00000000)?; // DATAW2: Prepare for read
        interface.write_word_32(0x4003408C, 0x00000000)?; // DATAW3: Prepare for read
        interface.write_word_32(0x40034090, 0x00000000)?; // DATAW4: Prepare for read
        interface.write_word_32(0x40034094, 0x00000000)?; // DATAW5: Prepare for read
        interface.write_word_32(0x40034098, 0x00000000)?; // DATAW6: Prepare for read
        interface.write_word_32(0x4003409C, 0x00000000)?; // DATAW7: Prepare for read

        interface.write_word_32(0x40034FE8, 0x0000000F)?; // Clear FLASH Controller Status (INT_CLR_STATUS)
        interface.write_word_32(0x40034000, 0x00000003)?; // Read single Flash Word (CMD_READ_SINGLE_WORD)
        interface.flush()?;

        let start = Instant::now();

        loop {
            let value = interface.read_word_32(0x40034FE0)?;
            if (value & 0x4) == 0x4 {
                break;
            }

            if start.elapsed() >= Duration::from_millis(100) {
                tracing::warn!("Failed: Wait for flash word read to finish");
                return Err(ArmError::Timeout);
            }
        }

        if (interface.read_word_32(0x4003_4fe0)? & 0xB) == 0 {
            tracing::info!("No Error reading Flash Word with Reset Vector");

            if (interface.read_word_32(0x400a_cffc)? & 0xC != 0x8)
                || (interface.read_word_32(0x400a_cff8)? & 0xC != 0x8)
            {
                // ENABLE_SECURE_CHECKING is set to restrictive mode, access secure addresses
                reset_vector_addr = 0x10000004;
            }

            reset_vector = interface.read_word_32(reset_vector_addr)?;
        }

        if reset_vector != 0xffff_ffff {
            tracing::info!("Breakpoint on user application reset vector: {reset_vector:#010x}");

            interface.write_word_32(0xE000_2008, reset_vector | 1)?;
            interface.write_word_32(0xE000_2000, 3)?;
        }

        if reset_vector == 0xffff_ffff {
            tracing::info!("Enable reset vector catch");

            let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

            demcr.set_vc_corereset(true);

            interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        }

        let _ = interface.read_word_32(Dhcsr::get_mmio_address())?;

        tracing::debug!("reset_catch_set -- done");

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        interface.write_word_32(0xE000_2008, 0x0)?;
        interface.write_word_32(0xE000_2000, 0x2)?;

        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        let mut result = interface.write_word_32(Aircr::get_mmio_address(), aircr.into());

        if result.is_ok() {
            result = interface.flush();
        }

        if let Err(e) = result {
            tracing::warn!("Error requesting reset: {:?}", e);
        }

        tracing::info!("Waiting after reset");
        thread::sleep(Duration::from_millis(10));

        let start = Instant::now();

        loop {
            if let Ok(v) = interface.read_word_32(Dhcsr::get_mmio_address()) {
                let dhcsr = Dhcsr(v);

                // Wait until the S_RESET_ST bit is cleared on a read
                if !dhcsr.s_reset_st() {
                    break;
                }
            }

            if start.elapsed() >= Duration::from_millis(500) {
                return wait_for_stop_after_reset(interface);
            }
        }

        Ok(())
    }
}

fn wait_for_stop_after_reset(memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
    tracing::info!("Wait for stop after reset");

    thread::sleep(Duration::from_millis(10));

    if memory.generic_status()?.enabled() {
        let dp = memory.fully_qualified_address().dp();
        enable_debug_mailbox(memory.get_dap_access()?, dp)?;
    }

    let start = Instant::now();

    tracing::debug!("Polling for reset");

    loop {
        if let Ok(v) = memory.read_word_32(Dhcsr::get_mmio_address()) {
            let dhcsr = Dhcsr(v);

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                break;
            }
        }

        if start.elapsed() >= Duration::from_millis(500) {
            return Err(ArmError::Timeout);
        }
    }

    let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::get_mmio_address())?);

    if !dhcsr.s_halt() {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);

        tracing::debug!("Force halt until finding a proper catch.");
        memory.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
    }

    Ok(())
}

fn enable_debug_mailbox(interface: &mut dyn DapAccess, dp: DpAddress) -> Result<(), ArmError> {
    tracing::info!("LPC55xx connect script start");

    let ap = FullyQualifiedApAddress::v1_with_dp(dp, 2);

    let status: IDR = interface
        .read_raw_ap_register(GenericAp::new(ap.clone()).ap_address(), IDR::ADDRESS)?
        .try_into()?;

    tracing::info!("APIDR: {:?}", status);
    tracing::info!("APIDR: 0x{:08X}", u32::from(status));

    // ADIv5 specification section B4.3.3: "Connection and line reset sequence" states that
    // in the reset state, reading DPIDR takes the target out of the reset state. Perform
    // such a read here in order to ensure the core is no longer in reset.
    let status: u32 = interface.read_raw_dp_register(dp, DPIDR::ADDRESS)?;

    tracing::info!("DPIDR: 0x{:08X}", status);

    // Active DebugMailbox
    interface.write_raw_ap_register(&ap, 0x0, 0x0000_0021)?;
    interface.flush()?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_millis(30));

    let _ = interface.read_raw_ap_register(&ap, 0)?;

    // Enter Debug session
    interface.write_raw_ap_register(&ap, 0x4, 0x0000_0007)?;
    interface.flush()?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_millis(30));

    let _ = interface.read_raw_ap_register(&ap, 8)?;

    tracing::info!("LPC55xx connect srcipt end");
    Ok(())
}

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
pub struct MIMXRT5xxS {
    family: MIMXRTFamily,
}

#[derive(PartialEq, Debug)]
/// MIMXRT Family Variants
pub enum MIMXRTFamily {
    /// MIMXRT5xxS Variants
    MIMXRT5,

    /// MIMXRT6xxS Variants
    MIMXRT6,
}

impl MIMXRT5xxS {
    const DWT_COMP0: u64 = 0xE0001020;
    const DWT_FUNCTION0: u64 = 0xE0001028;
    const SYSTEM_STICK_CALIB_ADDR: u32 = 0x50002034;
    const FLEXSPI_NOR_FLASH_HEADER_ADDR: u64 = 0x08000400;
    const FLEXSPI_NOR_FLASH_HEADER_MAGIC: u32 = 0x42464346;

    /// Create a sequence handle for the MIMXRT5xxS.
    pub fn create(family: MIMXRTFamily) -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self { family })
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
    fn wait_for_stop_after_reset(
        &self,
        probe: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        tracing::trace!("waiting for MIMXRT5xxS halt after reset");

        // Note: despite the name of this sequence in the CMSIS Pack, the
        // given implementation doesn't actually _wait_ for stop, and instead
        // just forces stopping itself. This is because there is no reliable
        // way to detect reset that works in all cases: the boot ROM might
        // jump into ISP mode, in which case we need to use the debug mailbox
        // to regain debug control.

        // Give bootloader time to do what it needs to do
        thread::sleep(Duration::from_millis(100));

        let ap = probe.fully_qualified_address();
        let dp = ap.dp();
        let start = Instant::now();
        while !self.csw_debug_ready(probe.get_dap_access()?, &ap)?
            && start.elapsed() < Duration::from_millis(300)
        {
            // Wait for either condition
        }
        let enabled_mailbox = self.enable_debug_mailbox(probe.get_dap_access()?, dp, &ap)?;

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
            if !self.csw_debug_ready(probe.get_dap_access()?, &ap)? {
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

    fn reset_flash(&self, interface: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        if self.family == MIMXRTFamily::MIMXRT5 {
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
            thread::sleep(Duration::from_millis(100));

            interface.write_word_32(0x40102010, 0)?; // PIO4_5 is an input
            interface.flush()?;
            thread::sleep(Duration::from_millis(100));
        } else {
            tracing::trace!("MIMXRT685-EVK FlexSPI flash reset (pulse PIO2_12)");

            // FIXME: We do this by twiddling PIO2_12, which is where the flash
            // reset pin is connected on MIMX685-EVK, but this code should not
            // make any assumptions about the evaluation board; how can we
            // generalize this so that the reset is configurable?
            //
            // See MIMX685-EVK schematics page 12 for details.
            interface.write_word_32(0x40021044, 1 << 2)?; // enable HSGPIO2 clock
            interface.write_word_32(0x40000074, 1 << 2)?; // take HSGPIO2 out of reset
            interface.write_word_32(0x40004130, 0x130)?; // full drive and pullup
            interface.write_word_32(0x40102008, 1 << 12)?; // PIO2_12 is an output
            interface.write_word_32(0x40102288, 1 << 12)?; // PIO2_12 is driven low
            thread::sleep(Duration::from_millis(100));

            interface.write_word_32(0x40102208, 1 << 12)?; // PIO2_12 is driven high
            interface.flush()?;
            thread::sleep(Duration::from_millis(100));
        }

        Ok(())
    }

    fn csw_debug_ready(
        &self,
        interface: &mut dyn DapAccess,
        ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        let csw = interface.read_raw_ap_register(ap, 0x00)?;

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
        interface: &mut dyn DapAccess,
        dp: DpAddress,
        mem_ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        // Check AHB-AP CSW DbgStatus to decide if need enable DebugMailbox
        if self.csw_debug_ready(interface, mem_ap)? {
            tracing::trace!("don't need to enable MIMXRT5xxS DebugMailbox");
            return Ok(false);
        }

        tracing::debug!("enabling MIMXRT5xxS DebugMailbox");

        let ap_addr = &FullyQualifiedApAddress::v1_with_dp(dp, 2);

        // CMSIS Pack implementation reads APIDR and DPIDR and passes each
        // to the "Message" function, but otherwise does nothing with those
        // results, so we skip that here.

        // Active DebugMailbox
        interface.write_raw_ap_register(ap_addr, 0x0, 0x00000021)?;
        thread::sleep(Duration::from_millis(30));
        interface.read_raw_ap_register(ap_addr, 0x0)?;

        // Enter Debug Session
        interface.write_raw_ap_register(ap_addr, 0x4, 0x00000007)?;
        thread::sleep(Duration::from_millis(30));
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
        let mut abort = Abort::default();
        abort.set_wderrclr(true);
        abort.set_orunerrclr(true);
        abort.set_stkcmpclr(true);
        abort.set_stkerrclr(true);

        tracing::trace!("MIMXRT5xxS debug port start");

        // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits of CTRL/STAT Register by write to ABORT register
        interface.write_dp_register(dp, abort)?;

        let dpidr: DPIDR = interface.read_dp_register(dp)?;

        // Switch to DP Register Bank 0
        interface.write_dp_register(dp, SelectV1(0))?;

        // Read DP CTRL/STAT Register and check if CSYSPWRUPACK and CDBGPWRUPACK bits are set
        let mut ctrl: Ctrl = interface.read_dp_register(dp)?;
        let powered_down = !ctrl.csyspwrupack() || !ctrl.cdbgpwrupack();
        if powered_down {
            tracing::trace!("MIMXRT5xxS is powered down, so requesting power-up");

            // Request Debug/System Power-Up
            ctrl.set_csyspwrupreq(true);
            ctrl.set_cdbgpwrupreq(true);
            interface.write_dp_register(dp, ctrl)?;

            // Wait for Power-Up Request to be acknowledged
            let start = Instant::now();
            loop {
                ctrl = interface.read_dp_register(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    break;
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
            }
        } else {
            tracing::trace!("MIMXRT5xxS debug port is already powered");
        }

        // SWD Specific Part of sequence
        // TODO: Should we skip this if we're not using SWD? How?
        // CMSIS Pack code uses: <control if="(__protocol &amp; 0xFFFF) == 2">
        if !dpidr.min() {
            // Init AP Transfer Mode, Transaction Counter, and Lane Mask (Normal Transfer Mode, Include all Byte Lanes)
            ctrl.set_mask_lane(0xF);
            interface.write_dp_register(dp, ctrl)?;

            // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits of CTRL/STAT Register by write to ABORT register
            interface.write_dp_register(dp, abort)?;

            let ap = FullyQualifiedApAddress::v1_with_dp(dp, 0);
            self.enable_debug_mailbox(interface, dp, &ap)?;
        }

        tracing::trace!("MIMXRT5xxS debug port start was successful");

        Ok(())
    }

    fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
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

    fn reset_hardware_deassert(
        &self,
        memory: &mut dyn ArmProbeInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        tracing::trace!("MIMXRT5xxS reset hardware deassert");
        let n_reset = Pins(0x80).0 as u32;

        let can_read_pins = memory.swj_pins(0, n_reset, 0)? != 0xffff_ffff;

        thread::sleep(Duration::from_millis(50));

        let mut assert_n_reset = || memory.swj_pins(n_reset, n_reset, 0);

        if can_read_pins {
            let start = Instant::now();
            let timeout_occured = || start.elapsed() > Duration::from_secs(1);

            while assert_n_reset()? & n_reset == 0 && !timeout_occured() {
                // Block until either condition passes
            }
        } else {
            assert_n_reset()?;
            thread::sleep(Duration::from_millis(100));
        }

        Ok(())
    }

    // "ResetHardware" intentionally omitted because the default implementation
    // seems equivalent to the one in the CMSIS-Pack.
}

/// Debug sequences for MIMXRT118x MCUs.
///
/// Currently only supports the Cortex M33.
#[derive(Debug)]
pub struct MIMXRT118x(());

bitfield! {
    /// SRC Reset Mask Register
    ///
    /// In probe-rs context used only to unmask AIRCR.SYSRESETREQ as a reset source
    ///
    /// Reference:
    /// `27.6.1.5` in `i.MX RT1180 Reference Manual, Rev. 6, 09/2024`
    #[derive(Copy, Clone)]
    pub struct Srmask(u32);
    impl Debug;
    /// Masks CM33 reset source
    ///
    /// `false` - the reset source is unmasked and active
    /// `true` - the reset source is masked and ignored
    ///
    /// Default value is `true`
    pub _, set_cm33_reset_mask : 8;
}

impl MIMXRT118x {
    fn new() -> Self {
        Self(())
    }

    /// Create a sequence handle for the MIMXRT118x.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self::new())
    }

    /// Necessary for AIRCR.SYSRESETREQ to not be a noop
    /// Reference:
    /// binaries/Scripts/RT1180_connect_M33_wake_M7.scp:53 in Linkserver
    /// `27.3.3.2` in `i.MX RT1180 Reference Manual, Rev. 6, 09/2024`
    fn clear_cm33_reset_mask(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        const SRC_SRMASK: u64 = 0x54460018;
        let mut srmask = Srmask(interface.read_word_32(SRC_SRMASK)?);
        srmask.set_cm33_reset_mask(false);
        tracing::trace!("Clearing the SRC.SRMASK.CM33_RESET_MASK mask...");
        interface.write_word_32(SRC_SRMASK, srmask.0)?;
        interface.flush()
    }
}

impl ArmDebugSequence for MIMXRT118x {
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: crate::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::trace!("MIMXRT118x reset system");

        self.clear_cm33_reset_mask(interface)?;
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        interface.write_word_32(Aircr::get_mmio_address(), aircr.into())?;
        interface.flush()?;
        tracing::trace!("Reset requested..");
        // After SYSRESETREQ, a _short_ sleep seems to be necessary. Otherwise debug interface enters some lock-up state.
        // NXP's Linkserver sends a vendor CMSIS DAP command to MCU-Link after reset which seems to be just a ~50ms sleep.
        // Doing the same seems to solve the issue ™️.
        //
        // It is pretty much the only reason why we cannot use a vanilla `cortex_m_reset_system`.
        thread::sleep(Duration::from_millis(50));

        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(500) {
            let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
                Ok(val) => Dhcsr(val),
                // Some combinations of debug probe and target (in
                // particular, hs-probe and ATSAMD21) result in
                // register read errors while the target is
                // resetting.
                Err(ArmError::AccessPort {
                    source: AccessPortError::RegisterRead { .. },
                    ..
                }) => continue,
                Err(err) => return Err(err),
            };
            if dhcsr.s_reset_st() {
                tracing::trace!("System reset was successful");
                return Ok(());
            }
        }

        tracing::error!("System reset timed out");
        Err(ArmError::Timeout)
    }

    fn allowed_access_ports(&self) -> Vec<u8> {
        // AP5 locks the whole DP if you try to read its IDR.
        vec![0, 1, 3, 4, 6]
    }
}
