//! Sequences for NXP chips that use ARMv7-M cores.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ap::{AccessPortError, ApAccess, MemoryAp, DRW, TAR},
        communication_interface::Initialized,
        core::armv7m::{Aircr, Dhcsr},
        dp::{Abort, Ctrl, DpAccess, Select},
        memory::adi_v5_memory_interface::ArmProbe,
        sequences::ArmDebugSequence,
        ApAddress, ArmCommunicationInterface, ArmError, DpAddress,
    },
    core::MemoryMappedRegister,
};

/// Start the debug port, and return if the device was (true) or wasn't (false)
/// powered down.
///
/// Note that this routine only supports SWD protocols. See the inline TODOs to
/// understand where JTAG support should go.
fn debug_port_start(
    interface: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
    select: Select,
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

        let mut timeout = true;

        while start.elapsed() < Duration::from_micros(100_0000) {
            let ctrl = interface.read_dp_register::<Ctrl>(dp)?;

            if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                timeout = false;
                break;
            }
        }

        if timeout {
            return Err(ArmError::Timeout);
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

/// Debug sequences for MIMXRT10xx MCUs.
///
/// In its current form, it uses no custom debug sequences. Instead, it ensures a reliable
/// reset sequence.
///
/// # On custom reset catch
///
/// Some tools use a custom reset catch that looks at the program image, finds the
/// reset vector, then places a breakpoint on that reset vector. This implementation
/// isn't doing that. That would be necessary if we don't control the kind of reset
/// that's happening. Since we're definitely using a SYSRESETREQ, we can rely on the
/// normal reset catch.
///
/// If the design changes such that the kind of reset isn't in our control, we'll
/// need to handle those cases.
#[derive(Debug)]
pub struct MIMXRT10xx(());

impl MIMXRT10xx {
    /// Create a sequence handle for the MIMXRT10xx.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }

    /// Runtime validation of core type.
    fn check_core_type(&self, core_type: crate::CoreType) -> Result<(), ArmError> {
        const EXPECTED: crate::CoreType = crate::CoreType::Armv7em;
        if core_type != EXPECTED {
            tracing::warn!(
                "MIMXRT10xx core type supplied as {core_type:?}, but the actual core is a {EXPECTED:?}"
            );
            // Not an issue right now. Warning because it's curious.
        }
        Ok(())
    }
}

impl ArmDebugSequence for MIMXRT10xx {
    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        core_type: crate::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.check_core_type(core_type)?;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        // Reset happens very quickly, and takes a bit. Ignore write and flush
        // errors that will occur due to the reset reaction.
        interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();
        interface.flush().ok();

        // Wait for the reset to finish...
        std::thread::sleep(Duration::from_millis(100));

        let start = Instant::now();
        while start.elapsed() < Duration::from_micros(50_0000) {
            let dhcsr = match interface.read_word_32(Dhcsr::get_mmio_address()) {
                Ok(val) => Dhcsr(val),
                Err(ArmError::AccessPort {
                    source:
                        AccessPortError::RegisterRead { .. } | AccessPortError::RegisterWrite { .. },
                    ..
                }) => {
                    // Some combinations of debug probe and target (in
                    // particular, hs-probe and ATSAMD21) result in
                    // register read errors while the target is
                    // resetting.
                    //
                    // See here for more info: https://github.com/probe-rs/probe-rs/pull/1174#issuecomment-1275568493
                    continue;
                }
                Err(err) => return Err(err),
            };

            // Wait until the S_RESET_ST bit is cleared on a read
            if !dhcsr.s_reset_st() {
                return Ok(());
            }
        }

        Err(ArmError::Timeout)
    }
}

/// Debug sequences for MIMXRT11xx MCUs.
///
/// Currently only supports the Cortex M7.
#[derive(Debug)]
pub struct MIMXRT11xx(());

impl MIMXRT11xx {
    /// Create a sequence handle for the MIMXRT10xx.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }

    fn prepare_cm7_trap_code(
        &self,
        ap: MemoryAp,
        interface: &mut ArmCommunicationInterface<Initialized>,
    ) -> Result<(), ArmError> {
        const START: u32 = 0x2001FF00;
        const IOMUX_LPSR_GPR26: u32 = 0x40C0C068;

        interface.write_ap_register(ap, TAR { address: START })?;
        interface.write_ap_register(ap, DRW { data: START + 0x20 })?;

        interface.write_ap_register(ap, TAR { address: START + 4 })?;
        interface.write_ap_register(ap, DRW { data: 0x23105 })?;

        interface.write_ap_register(
            ap,
            TAR {
                address: IOMUX_LPSR_GPR26,
            },
        )?;
        interface.write_ap_register(ap, DRW { data: START >> 7 })?;
        Ok(())
    }

    fn prepare_cm4_trap_code(
        &self,
        ap: MemoryAp,
        interface: &mut ArmCommunicationInterface<Initialized>,
    ) -> Result<(), ArmError> {
        const START: u32 = 0x20250000;
        const IOMUX_LPSR_GPR0: u32 = 0x40c0c000;
        const IOMUX_LPSR_GPR1: u32 = 0x40c0c004;
        interface.write_ap_register(ap, TAR { address: START })?;
        interface.write_ap_register(ap, DRW { data: START + 0x20 })?;

        interface.write_ap_register(ap, TAR { address: START + 4 })?;
        interface.write_ap_register(ap, DRW { data: 0x23F041 })?;

        interface.write_ap_register(
            ap,
            TAR {
                address: IOMUX_LPSR_GPR0,
            },
        )?;
        interface.write_ap_register(
            ap,
            DRW {
                data: START & 0xFFFF,
            },
        )?;

        interface.write_ap_register(
            ap,
            TAR {
                address: IOMUX_LPSR_GPR1,
            },
        )?;
        interface.write_ap_register(ap, DRW { data: START >> 16 })?;
        Ok(())
    }

    fn release_cm4(
        &self,
        ap: MemoryAp,
        interface: &mut ArmCommunicationInterface<Initialized>,
    ) -> Result<(), ArmError> {
        const SRC_SCR: u32 = 0x40c04000;
        interface.write_ap_register(ap, TAR { address: SRC_SCR })?;
        interface.write_ap_register(ap, DRW { data: 1 })?;
        Ok(())
    }

    fn change_reset_modes(
        &self,
        ap: MemoryAp,
        interface: &mut ArmCommunicationInterface<Initialized>,
    ) -> Result<(), ArmError> {
        const SRC_SBMR: u32 = 0x40c04004;
        interface.write_ap_register(ap, TAR { address: SRC_SBMR })?;
        let DRW { data: mut src_sbmr } = interface.read_ap_register(ap)?;
        src_sbmr |= 0xF << 10; // Puts both cores into "do not reset."
        interface.write_ap_register(ap, DRW { data: src_sbmr })?;
        Ok(())
    }
}

impl ArmDebugSequence for MIMXRT11xx {
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::debug!("debug_port_start");
        // Note that debug_port_start only supports SWD protocols,
        // which means the MIMXRT11xx only supports SWD right now.
        // See its documentation and TODOs.
        self::debug_port_start(interface, dp, Select(0))?;

        let ap = ApAddress { dp, ap: 0 };
        let ap = MemoryAp::new(ap);

        tracing::debug!("Prepare trap code for Cortex M7");
        self.prepare_cm7_trap_code(ap, interface)?;

        tracing::debug!("Prepare trap code for Cortex M4");
        self.prepare_cm4_trap_code(ap, interface)?;

        tracing::debug!("Release the CM4");
        self.release_cm4(ap, interface)?;

        tracing::debug!("Change reset mode of both cores");
        self.change_reset_modes(ap, interface)?;
        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        _: crate::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        // It's unpredictable to VECTRESET a core if it's not halted and
        // in debug state.
        tracing::debug!("Halting MIMXRT11xx core before VECTRESET");
        let mut dhcsr = Dhcsr(0);
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();

        interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        std::thread::sleep(Duration::from_millis(100));

        // Initial testing showed that a SYSRESET (the default reset approach)
        // can result in an unreliable programming sequence, particularly if
        // the target we're reprogramming is interrupting / excepting.
        //
        // The debug port setup (above) will trap the core(s) after this VECRESET.
        // Once that trap happens, we're ready to debug / flash.
        tracing::debug!("Resetting MIMXRT11xx with VECTRESET");
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_vectreset(true);

        interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();
        interface.flush().ok();

        std::thread::sleep(Duration::from_millis(100));

        interface.read_word_32(Dhcsr::get_mmio_address())?;
        Ok(())
    }
}
