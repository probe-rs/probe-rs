//! Sequences for NXP MCX chips.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitfield::BitMut;
use probe_rs_target::CoreType;

use crate::{
    architecture::arm::{
        ap::{ApRegister, CSW, IDR},
        armv8m::{self},
        communication_interface::Initialized,
        dp::{Abort, Ctrl, DpAccess, DpAddress, DpRegister, SelectV1, DPIDR},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
        ArmCommunicationInterface, ArmError, DapAccess, FullyQualifiedApAddress,
    },
    probe::WireProtocol,
    MemoryMappedRegister,
};

/// Debug sequences for MCX family MCUs.
#[derive(Debug)]
pub struct MCX {
    _variant: String, // This member is useful for the other MCX Series MCUs, keep it for future.
}

/// MCX Family Variants
#[derive(Debug)]
pub enum MCXFamily {
    /// MCXAxxx Variants
    MCXA,
}

impl MCX {
    /// Create a sequence handle for the MCX MCUs.
    pub fn create(variant: String) -> Arc<dyn ArmDebugSequence> {
        Arc::new(MCX { _variant: variant })
    }

    fn enable_debug_mailbox(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<bool, ArmError> {
        let csw = interface
            .read_raw_ap_register(&FullyQualifiedApAddress::v1_with_dp(dp, 0), CSW::ADDRESS)?;
        if csw & 0x40 != 0 {
            return Ok(false);
        }

        tracing::info!("MCX connect script start");

        let ap = FullyQualifiedApAddress::v1_with_dp(dp, 1);
        let status: IDR = interface
            .read_raw_ap_register(&ap, IDR::ADDRESS)?
            .try_into()?;
        tracing::info!("APIDR: {:?}", status);
        tracing::info!("APIDR: 0x{:08X}", u32::from(status));

        let status: DPIDR = interface
            .read_raw_dp_register(dp, DPIDR::ADDRESS)?
            .try_into()?;
        tracing::info!("DPIDR: {:?}", status);
        tracing::info!("DPIDR: 0x{:08X}", u32::from(status));

        // Active DebugMailbox
        interface.write_raw_ap_register(&ap, 0x0, 0x0000_0021)?;
        thread::sleep(Duration::from_millis(30));
        interface.read_raw_ap_register(&ap, 0x0)?;

        // Enter Debug Session
        interface.write_raw_ap_register(&ap, 0x4, 0x0000_0007)?;
        thread::sleep(Duration::from_millis(30));
        interface.read_raw_ap_register(&ap, 0x0)?;

        Ok(true)
    }

    fn wait_for_stop_after_reset(
        &self,
        probe: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        // Give bootloader time to do what it needs to do
        thread::sleep(Duration::from_millis(100));

        let ap = probe.fully_qualified_address();
        let dp = ap.dp();
        self.enable_debug_mailbox(probe.get_dap_access()?, dp)?;

        // Halt the core in case it didn't stop at a breakpoint
        let mut dhcsr = armv8m::Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        probe.write_word_32(armv8m::Dhcsr::get_mmio_address(), dhcsr.into())?;

        // Clear watch point
        probe.write_word_32(0xE0001020, 0x0)?;
        probe.write_word_32(0xE0001028, 0x0)?;
        probe.write_word_32(0xE0001030, 0x0)?;
        probe.write_word_32(0xE0001038, 0x0)?;

        // Clear XPSR to avoid undefined instruction fault caused by IT/ICI
        probe.write_word_32(0xE000EDF8, 0x01000000)?;
        probe.write_word_32(0xE000EDF4, 0x00010010)?;
        // Set MSPLIM to 0
        probe.write_word_32(0xE000EDF8, 0x00000000)?;
        probe.write_word_32(0xE000EDF4, 0x0001001C)?;

        let start = Instant::now();
        loop {
            let dhcsr: armv8m::Dhcsr = probe
                .read_word_32(armv8m::Dhcsr::get_mmio_address())?
                .into();
            if !dhcsr.s_reset_st() {
                break;
            }
            if start.elapsed() > Duration::from_millis(500) {
                return Err(ArmError::Timeout);
            }
        }

        let dhcsr: armv8m::Dhcsr = probe
            .read_word_32(armv8m::Dhcsr::get_mmio_address())?
            .into();
        if !dhcsr.s_halt() {
            let mut dhcsr = armv8m::Dhcsr(0);
            dhcsr.enable_write();
            dhcsr.set_c_debugen(true);
            dhcsr.set_c_halt(true);
            probe.write_word_32(armv8m::Dhcsr::get_mmio_address(), dhcsr.into())?;
        }

        Ok(())
    }
}

impl ArmDebugSequence for MCX {
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("debug_port_start");

        // Switch to DP Register Bank 0
        interface.write_dp_register(dp, SelectV1(0))?;

        // Read DP CTRL/STAT Register and check if CSYSPWRUPACK and CDBGPWRUPACK bits are set
        let ctrl: Ctrl = interface.read_dp_register(dp)?;
        let powered_down = !(ctrl.csyspwrupack() && ctrl.cdbgpwrupack());
        tracing::info!("powered_down: {}", powered_down);

        if !powered_down {
            return Ok(());
        }

        // Request Debug/System Power-Up
        let mut ctrl = Ctrl(0);
        ctrl.set_csyspwrupreq(true);
        ctrl.set_cdbgpwrupreq(true);
        interface.write_dp_register(dp, ctrl)?;

        // Wait for Power-Up request to be acknowledged
        let start = Instant::now();
        loop {
            ctrl = interface.read_dp_register(dp)?;
            if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                break;
            }
            if start.elapsed() >= Duration::from_secs(1) {
                tracing::warn!("wait for power-up request to be acknowledged timeout!");
                return Err(ArmError::Timeout);
            }
        }

        if let Some(protocol) = interface.probe_mut().active_protocol() {
            match protocol {
                WireProtocol::Jtag => {
                    // Init AP Transfer Mode, Transaction Counter, and
                    // Lane Mask (Normal Transfer Mode, Include  all Byte Lanes)
                    // Additionally clear STICKYORUN, STICKCMP, and STICKYERR bits
                    let mut ctrl = Ctrl(0);
                    ctrl.set_csyspwrupreq(true);
                    ctrl.set_cdbgpwrupreq(true);
                    ctrl.set_mask_lane(0b1111);
                    ctrl.set_bit(1, true);
                    ctrl.set_bit(4, true);
                    ctrl.set_bit(5, true);
                    interface.write_dp_register(dp, ctrl)?;
                }
                WireProtocol::Swd => {
                    // Init AP Transfer Mode, Transaction Counter, and
                    // Lane Mask (Normal Transfer Mode, Include  all Byte Lanes)
                    let mut ctrl = Ctrl(0);
                    ctrl.set_csyspwrupreq(true);
                    ctrl.set_cdbgpwrupreq(true);
                    ctrl.set_mask_lane(0b1111);
                    interface.write_dp_register(dp, ctrl)?;

                    // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits
                    // of CTRL/STAT Register by write to ABORT register
                    let mut abort = Abort(0);
                    abort.set_wderrclr(true);
                    abort.set_orunerrclr(true);
                    abort.set_stkcmpclr(true);
                    abort.set_stkerrclr(true);
                    interface.write_dp_register(dp, abort)?;
                }
            }

            self.enable_debug_mailbox(interface, dp)?;
        }

        Ok(())
    }

    fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        if core_type != CoreType::Armv8m {
            return Err(ArmError::ArchitectureRequired(&["ARMv8"]));
        }

        // Halt the core
        let mut dhcsr = armv8m::Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        probe.write_word_32(armv8m::Dhcsr::get_mmio_address(), dhcsr.into())?;
        probe.flush()?;

        // clear VECTOR CATCH and set TRCENA
        let mut demcr: armv8m::Demcr = probe
            .read_word_32(armv8m::Demcr::get_mmio_address())?
            .into();
        demcr.set_trcena(true);
        probe.write_word_32(armv8m::Demcr::get_mmio_address(), demcr.into())?;
        probe.flush()?;

        probe.write_word_32(0xE0001020, 0x00000000)?;
        probe.write_word_32(0xE0001028, 0xF0000412)?;
        probe.write_word_32(0xE0001030, 0x000FFFFF)?;
        probe.write_word_32(0xE0001038, 0xF0000403)?;

        let mut aircr = armv8m::Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let _ = probe.write_word_32(armv8m::Aircr::get_mmio_address(), aircr.into());
        let _ = self.wait_for_stop_after_reset(probe);

        Ok(())
    }
}
