//! Sequences for NXP chips.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use crate::{
    architecture::arm::{
        ap::{AccessPort, ApAccess, GenericAp, MemoryAp, DRW, IDR, TAR},
        communication_interface::{FlushableArmAccess, Initialized},
        core::armv7m::{Aircr, Demcr, Dhcsr},
        dp::{Abort, Ctrl, DpAccess, Select, DPIDR},
        memory::adi_v5_memory_interface::ArmProbe,
        ApAddress, ArmCommunicationInterface, ArmError, DapAccess, DpAddress,
    },
    core::MemoryMappedRegister,
};

use super::ArmDebugSequence;

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

/// The sequence handle for the LPC55S69.
pub struct LPC55S69(());

impl LPC55S69 {
    /// Create a sequence handle for the LPC55S69.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self(()))
    }
}

impl ArmDebugSequence for LPC55S69 {
    fn debug_port_start(
        &self,
        interface: &mut ArmCommunicationInterface<Initialized>,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("debug_port_start");

        let powered_down = self::debug_port_start(interface, dp, Select(0))?;

        if powered_down {
            enable_debug_mailbox(interface, dp)?;
        }

        Ok(())
    }

    fn reset_catch_set(
        &self,
        interface: &mut dyn ArmProbe,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut reset_vector = 0xffff_ffff;
        let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::ADDRESS, demcr.into())?;

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

        let mut timeout = true;

        while start.elapsed() < Duration::from_micros(10_0000) {
            let value = interface.read_word_32(0x40034FE0)?;

            if (value & 0x4) == 0x4 {
                timeout = false;
                break;
            }
        }

        if timeout {
            tracing::warn!("Failed: Wait for flash word read to finish");
            return Err(ArmError::Timeout);
        }

        if (interface.read_word_32(0x4003_4fe0)? & 0xB) == 0 {
            tracing::info!("No Error reading Flash Word with Reset Vector");

            reset_vector = interface.read_word_32(0x0000_0004)?;
        }

        if reset_vector != 0xffff_ffff {
            tracing::info!("Breakpoint on user application reset vector");

            interface.write_word_32(0xE000_2008, reset_vector | 1)?;
            interface.write_word_32(0xE000_2000, 3)?;
        }

        if reset_vector == 0xffff_ffff {
            tracing::info!("Enable reset vector catch");

            let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

            demcr.set_vc_corereset(true);

            interface.write_word_32(Demcr::ADDRESS, demcr.into())?;
        }

        let _ = interface.read_word_32(Dhcsr::ADDRESS)?;

        tracing::debug!("reset_catch_set -- done");

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        interface: &mut dyn ArmProbe,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        interface.write_word_32(0xE000_2008, 0x0)?;
        interface.write_word_32(0xE000_2000, 0x2)?;

        let mut demcr = Demcr(interface.read_word_32(Demcr::ADDRESS)?);

        demcr.set_vc_corereset(false);

        interface.write_word_32(Demcr::ADDRESS, demcr.into())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmProbe,
        _core_type: crate::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);

        let mut result = interface.write_word_32(Aircr::ADDRESS, aircr.into());

        if result.is_ok() {
            result = interface.flush();
        }

        if let Err(e) = result {
            tracing::debug!("Error requesting reset: {:?}", e);
        }

        tracing::info!("Waiting after reset");
        thread::sleep(Duration::from_millis(10));

        wait_for_stop_after_reset(interface)
    }
}

fn wait_for_stop_after_reset(memory: &mut dyn ArmProbe) -> Result<(), ArmError> {
    tracing::info!("Wait for stop after reset");

    thread::sleep(Duration::from_millis(10));

    let dp = memory.ap().ap_address().dp;
    let interface = memory.get_arm_communication_interface()?;

    enable_debug_mailbox(interface, dp)?;

    let mut timeout = true;

    let start = Instant::now();

    tracing::info!("Polling for reset");

    while start.elapsed() < Duration::from_micros(50_0000) {
        let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

        if !dhcsr.s_reset_st() {
            timeout = false;
            break;
        }
    }

    if timeout {
        return Err(ArmError::Timeout);
    }

    let dhcsr = Dhcsr(memory.read_word_32(Dhcsr::ADDRESS)?);

    if !dhcsr.s_halt() {
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);

        memory.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;
    }

    Ok(())
}

fn enable_debug_mailbox(
    interface: &mut ArmCommunicationInterface<Initialized>,
    dp: DpAddress,
) -> Result<(), ArmError> {
    tracing::info!("LPC55xx connect srcipt start");

    let ap = ApAddress { dp, ap: 2 };

    let status: IDR = interface.read_ap_register(GenericAp::new(ap))?;

    tracing::info!("APIDR: {:?}", status);
    tracing::info!("APIDR: 0x{:08X}", u32::from(status));

    let status: u32 = interface.read_dp_register::<DPIDR>(dp)?.into();

    tracing::info!("DPIDR: 0x{:08X}", status);

    // Active DebugMailbox
    interface.write_raw_ap_register(ap, 0x0, 0x0000_0021)?;
    interface.flush()?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_micros(30000));

    let _ = interface.read_raw_ap_register(ap, 0)?;

    // Enter Debug session
    interface.write_raw_ap_register(ap, 0x4, 0x0000_0007)?;
    interface.flush()?;

    // DAP_Delay(30000)
    thread::sleep(Duration::from_micros(30000));

    let _ = interface.read_raw_ap_register(ap, 8)?;

    tracing::info!("LPC55xx connect srcipt end");
    Ok(())
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
        interface.write_word_32(Aircr::ADDRESS, aircr.into()).ok();
        interface.flush().ok();

        // Wait for the reset to finish...
        std::thread::sleep(Duration::from_millis(100));

        // Clear the status bit. This read shouldn't fail.
        interface.read_word_32(Dhcsr::ADDRESS)?;
        Ok(())
    }
}

/// Debug sequences for MIMXRT11xx MCUs.
///
/// Currently only supports the Cortex M7.
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

        interface.write_word_32(Dhcsr::ADDRESS, dhcsr.into())?;
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

        interface.write_word_32(Aircr::ADDRESS, aircr.into()).ok();
        interface.flush().ok();

        std::thread::sleep(Duration::from_millis(100));

        interface.read_word_32(Dhcsr::ADDRESS)?;
        Ok(())
    }
}
