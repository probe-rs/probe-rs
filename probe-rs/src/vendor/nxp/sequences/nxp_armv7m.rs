//! Sequences for NXP chips that use ARMv7-M cores.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use probe_rs_target::Chip;

use crate::{
    architecture::arm::{
        ArmDebugInterface, ArmError, FullyQualifiedApAddress, Pins,
        armv7m::{Demcr, FpCtrl, FpRev2CompX},
        communication_interface::DapProbe,
        core::{
            armv7m::{Aircr, Dhcsr},
            registers::cortex_m::PC,
        },
        dp::DpAddress,
        memory::ArmMemoryInterface,
        sequences::{self, ArmDebugSequence, ArmDebugSequenceError},
    },
    core::MemoryMappedRegister,
};

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
pub struct MIMXRT10xx {
    /// We're always catching the MCU at a watchpoint
    /// in the boot ROM. "Not catching" means that we'll
    /// release it after it hits the watchpoint.
    simulate_reset_catch: AtomicBool,
}

impl MIMXRT10xx {
    /// Create a sequence handle for the MIMXRT10xx.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self {
            simulate_reset_catch: AtomicBool::new(false),
        })
    }

    /// Halt or unhalt the core.
    fn halt(&self, probe: &mut dyn ArmMemoryInterface, halt: bool) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(probe.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_halt(halt);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();

        probe.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        probe.flush()?;

        self.wait_for_halt(probe, halt)?;

        Ok(())
    }

    /// Use the boot fuse configuration for FlexRAM.
    ///
    /// If the user changed the FlexRAM configuration in software,
    /// this will undo that configuration, preferring the system's POR
    /// FlexRAM state.
    ///
    /// This function may change the processor's memory map, which may
    /// cause problems for any running firmware.  Halt the processor
    /// before calling this function.
    fn use_boot_fuses_for_flexram(
        &self,
        probe: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        const IOMUXC_GPR_GPR16: u64 = 0x400A_C040;
        const FLEXRAM_BANK_CFG_SEL_MASK: u32 = 1 << 2;
        let mut gpr16 = probe.read_word_32(IOMUXC_GPR_GPR16)?;
        gpr16 &= !FLEXRAM_BANK_CFG_SEL_MASK;
        probe.write_word_32(IOMUXC_GPR_GPR16, gpr16)?;
        probe.flush()?;
        Ok(())
    }

    /// Wait for the MCU to signal it's halted.
    fn wait_for_halt(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        halt: bool,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        let action = if halt { "halt" } else { "unhalt" };
        while Dhcsr(probe.read_word_32(Dhcsr::get_mmio_address())?).s_halt() != halt {
            if start.elapsed() > Duration::from_millis(100) {
                tracing::debug!("Exceeded timeout while waiting for core to {action}");
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }
}

impl ArmDebugSequence for MIMXRT10xx {
    fn reset_catch_set(
        &self,
        _: &mut dyn ArmMemoryInterface,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn reset_catch_clear(
        &self,
        _: &mut dyn ArmMemoryInterface,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _: crate::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        tracing::debug!("Halting MCU before changing FlexRAM layout");
        self.halt(interface, true)?;

        // OK to perform before the reset, since the configuration
        // persists beyond the reset.
        tracing::debug!("Setting FlexRAM layout");
        self.use_boot_fuses_for_flexram(interface)?;

        tracing::debug!("Enabling DWT to set a watchpoint");
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        let trcena = demcr.trcena();
        demcr.set_trcena(true);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.0)?;

        // Catching the MCU here helps RAM loading reliability.
        // The boot ROM sets up just enough of the MCU for us,
        // and we catch it as it tries to figure out the boot
        // configuration. If we're not changing execution context
        // after the fact, this is a no-op.
        tracing::debug!("Installing watchpoint to catch boot ROM SRC_SBMR1 read");
        const DWT_COMP0: u64 = 0xE000_1020;
        const DWT_MASK0: u64 = 0xE000_1024;
        const DWT_FUNCTION0: u64 = 0xE000_1028;
        const DWT_FUNCTION_DATAVSIZE_WORD: u32 = 0b10 << 10;
        const DWT_FUNCTION_DEBUG_DATA_RW: u32 = 0b0111;
        const SRC_SBMR1: u32 = 0x400F_8004;
        interface.write_word_32(DWT_COMP0, SRC_SBMR1)?;
        interface.write_word_32(DWT_MASK0, 0)?;
        interface.write_word_32(
            DWT_FUNCTION0,
            DWT_FUNCTION_DATAVSIZE_WORD | DWT_FUNCTION_DEBUG_DATA_RW,
        )?;

        interface.flush()?;

        // Do the usual reset. The watchpoint persists across the
        // reset.
        tracing::debug!("Performing the standard Cortex-M system reset");
        sequences::cortex_m_reset_system(interface)?;

        // Wait for that watchpoint to hit.
        tracing::debug!("Waiting for watchpoint to hit");
        self.wait_for_halt(interface, true)?;

        // Clean up after ourselves.
        tracing::debug!("Cleaning up watchpoints");
        interface.write_word_32(DWT_COMP0, 0)?;
        interface.write_word_32(DWT_FUNCTION0, 0)?;

        // Keep whatever tracing selection the system
        // previously had.
        let mut demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);
        demcr.set_trcena(trcena);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.0)?;

        interface.flush()?;

        // Unhalt if we're not catching the reset.
        if !self.simulate_reset_catch.load(Ordering::Relaxed) {
            self.halt(interface, false)?;
        }

        Ok(())
    }
}

/// Backwards-compatible debug sequence for MIMXRT1170 MCUs.
#[deprecated(note = "Prefer MIMXRT11xx, which supports 1170 and 1160 targets")]
pub type MIMXRT117x = MIMXRT11xx;

/// Debug sequences for MIMXRT1170 / MIMXRT1160 MCUs.
///
/// Currently only supports the Cortex M7. In fact, if you try to interact with the Cortex M4,
/// you'll have a bad time: its access port doesn't appear until it's released from reset!
/// For the time being, you can only do things through the CM7.
#[derive(Debug)]
pub struct MIMXRT11xx {
    /// Given the reset we're performing, we won't be able to perform
    /// a normal vector catch. (The boot ROM doesn't care about us.)
    /// We'll simulate that behavior for the user.
    simulate_reset_catch: AtomicBool,
}

impl MIMXRT11xx {
    /// System reset controller base address.
    const SRC: u64 = 0x40C0_4000;
    /// SRC reset mode register.
    const SRC_SRMR: u64 = Self::SRC + 4;

    fn new() -> Self {
        Self {
            simulate_reset_catch: AtomicBool::new(false),
        }
    }

    /// Create a sequence handle for the MIMXRT1170 / MIMXRT1160.
    pub fn create() -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self::new())
    }

    /// To ensure we affect a system reset, clear the mask that would prevent
    /// a response to the CM7's SYSRESETREQ.
    fn clear_src_srmr_mask(&self, probe: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        let mut srmr = probe.read_word_32(Self::SRC_SRMR)?;
        tracing::debug!("SRC_SRMR: {srmr:#010X}. Clearing the M7REQ_RESET_MODE mask...");
        srmr &= !(0b11 << 12);
        probe.write_word_32(Self::SRC_SRMR, srmr)?;
        probe.flush()?;
        Ok(())
    }

    /// Halt or unhalt the core.
    fn halt(&self, probe: &mut dyn ArmMemoryInterface, halt: bool) -> Result<(), ArmError> {
        let mut dhcsr = Dhcsr(probe.read_word_32(Dhcsr::get_mmio_address())?);
        dhcsr.set_c_halt(halt);
        dhcsr.set_c_debugen(true);
        dhcsr.enable_write();

        probe.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        probe.flush()?;

        let start = Instant::now();
        let action = if halt { "halt" } else { "unhalt" };

        while Dhcsr(probe.read_word_32(Dhcsr::get_mmio_address())?).s_halt() != halt {
            if start.elapsed() > Duration::from_millis(100) {
                tracing::debug!("Exceeded timeout while waiting for the core to {action}");
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    /// Poll the AP's status until it can accept transfers.
    fn wait_for_enable(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        timeout: Duration,
    ) -> Result<(), ArmError> {
        let start = Instant::now();
        let mut errors = 0usize;
        let mut disables = 0usize;

        loop {
            match probe.generic_status() {
                Ok(csw) if csw.DeviceEn => {
                    tracing::debug!(
                        "Device enabled after {}ms with {errors} errors and {disables} invalid statuses",
                        start.elapsed().as_millis()
                    );
                    return Ok(());
                }
                Ok(_) => disables += 1,
                Err(_) => errors += 1,
            }

            if start.elapsed() > timeout {
                tracing::debug!(
                    "Exceeded {}ms timeout while waiting for enable with {errors} errors and {disables} invalid statuses",
                    timeout.as_millis()
                );
                return Err(ArmError::Timeout);
            }

            thread::sleep(Duration::from_millis(1));
        }
    }

    /// Assumes that the core is halted.
    fn read_core_reg(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        reg: crate::core::registers::CoreRegister,
    ) -> Result<u32, ArmError> {
        crate::architecture::arm::core::cortex_m::read_core_reg(probe, reg.into())
    }

    /// Assumes that the core is halted.
    fn write_core_reg(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        reg: crate::core::registers::CoreRegister,
        value: u32,
    ) -> Result<(), ArmError> {
        crate::architecture::arm::core::cortex_m::write_core_reg(probe, reg.into(), value)?;
        probe.flush()?;
        Ok(())
    }

    /// Ensure that the program counter's contents match `expected`.
    ///
    /// Assumes that the core is halted.
    fn check_pc(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        expected: u32,
    ) -> Result<(), ArmDebugSequenceError> {
        let pc = self
            .read_core_reg(probe, PC)
            .map_err(|err| ArmDebugSequenceError::SequenceSpecific(err.into()))?;
        if pc != expected {
            let err = format!(
                "The Cortex M7 should be at address {expected:#010X} but it's at {pc:#010X}"
            );
            return Err(ArmDebugSequenceError::SequenceSpecific(err.into()));
        }
        Ok(())
    }

    /// When the boot ROM detects a reset due to SYSRESETREQ, it spins
    /// at this location. It appears that this spinning location is after
    /// the boot ROM has done its useful work (like turn on clocks, prepare
    /// FlexSPI configuration blocks), but before it jumps into the program.
    const BOOT_ROM_SPIN_ADDRESS: u32 = 0x00223104;

    /// Returns the reset handler address contained in the NVM program image.
    ///
    /// We might not find that reset handler. In that case, return `None`.
    fn find_flexspi_image_reset_handler(
        &self,
        probe: &mut dyn ArmMemoryInterface,
    ) -> Result<Option<u32>, ArmError> {
        /// Assumed by today's in-tree target definition.
        const FLEXSPI1: u64 = 0x30000000;
        /// A well-formed FlexSPI program has its image vector table at this offset in flash.
        const IVT: u64 = FLEXSPI1 + 0x1000;
        tracing::debug!("Assuming that your CM7's program is in FlexSPI1 at {FLEXSPI1:#010X}");

        // Make sure the IVT header looks reasonable.
        //
        // See 10.7.1.1 Image vector table structure in the 1170 RM (Rev 2).
        // If it doesn't look reasonable, we assume that FlexSPI is inaccessible.
        let ivt_header = probe.read_word_32(IVT)?;
        tracing::debug!("IVT Header: {ivt_header:#010X}");

        if ivt_header & 0xFF != 0xD1 {
            tracing::debug!("IVT tag is incorrect! Expected 0xD1 in {ivt_header:#010X}");
            return Ok(None);
        }

        if (ivt_header >> 8) & 0xFFFF != 0x2000 {
            tracing::debug!("IVT length is incorrect! {ivt_header:#010X}");
            return Ok(None);
        }

        let ivt_version = ivt_header >> 24;
        if !(0x40..=0x45).contains(&ivt_version) {
            tracing::debug!("IVT version is invalid! {ivt_header:#010X}");
            return Ok(None);
        }

        // IVT versions 4.0 (0x40) are documented as containing the "entry point."
        // But in practice, this seems to be the pointer to the vector table. IVT
        // versions 4.1 and 4.3 (0x41, 0x43) appear to truly use the reset handler, not
        // the vector table. I can't find any documentation on this, so this comes from
        // some local testing. We assume that 4.0 is the outlier, and that all versions
        // above it use the same approach.
        let reset_handler = if ivt_version == 0x40 {
            // The address of the vector table is immediately behind the IVT header.
            let vector_table = probe.read_word_32(IVT + 4)?;
            tracing::debug!("Vector table address: {vector_table:#010X}");

            // The vector table starts with the stack pointer. Then the
            // reset handle is immediately behind that.
            probe.read_word_32(u64::from(vector_table) + 4u64)?
        } else {
            // The reset handler immediately follows the IVT header.
            probe.read_word_32(IVT + 4)?
        };

        tracing::debug!("Reset handler: {reset_handler:#010X}");
        if reset_handler & 1 == 0 {
            tracing::debug!(
                "Is your reset handler actually a function address? Where's its thumb bit?"
            );
            return Ok(None);
        }

        Ok(Some(reset_handler))
    }

    /// See documentation for [`MIMXRT10xx::use_boot_fuses_for_flexram`].
    fn use_boot_fuses_for_flexram(
        &self,
        probe: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        const IOMUXC_GPR_GPR16: u64 = 0x400E_4040;
        const FLEXRAM_BANK_CFG_SEL_MASK: u32 = 1 << 2;
        let mut gpr16 = probe.read_word_32(IOMUXC_GPR_GPR16)?;
        gpr16 &= !FLEXRAM_BANK_CFG_SEL_MASK;
        probe.write_word_32(IOMUXC_GPR_GPR16, gpr16)?;
        probe.flush()?;
        Ok(())
    }
}

impl ArmDebugSequence for MIMXRT11xx {
    fn reset_catch_set(
        &self,
        _: &mut dyn ArmMemoryInterface,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn reset_catch_clear(
        &self,
        _: &mut dyn ArmMemoryInterface,
        _: probe_rs_target::CoreType,
        _: Option<u64>,
    ) -> Result<(), ArmError> {
        self.simulate_reset_catch.store(false, Ordering::Relaxed);
        Ok(())
    }

    fn reset_system(
        &self,
        probe: &mut dyn ArmMemoryInterface,
        core_type: probe_rs_target::CoreType,
        debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // OK to perform before the reset, since the configuration
        // persists beyond the reset.
        self.halt(probe, true)?;
        self.use_boot_fuses_for_flexram(probe)?;

        // Cache debug system state that may be lost across the reset.
        let debug_cache = DebugCache::from_target(probe)?;

        // Make sure that the CM7's SYSRESETREQ isn't ignored by the system
        // reset controller.
        self.clear_src_srmr_mask(probe)?;

        // Affect a SYSRESETREQ through the CM7 to reset the entire system.
        //
        // For more information on the SYSRESETREQ response, consult the system
        // reset controller (SRC) section of the reference manual. This is a
        // convenient way to perform a whole-system reset.
        //
        // Another approach to perform this reset: iterate through all SRC slice controls,
        // and manually reset them. That should be close to SYSRESETREQ. However, it seems
        // that there are no slice controls for CM4MEM (LMEM) and CM7MEM (FlexRAM)
        // so you might not be able to affect a reset on those two domains.
        //
        // If you scan through the slices, you'll notice that the M7CORE and M7DEBUG are
        // different slices. You'll think "I can perform a reset through the SRC that hits
        // all slices except the M*DEBUG slices. This would preserve debugging and I won't
        // have to re-initialize the debug port!" I could not get that to work; if I did a
        // reset through SRC_CTRL_M7CORE, I found that I still needed to re-initialize the
        // debug port after the reset. Maybe I did something wrong.
        //
        // We're about to lose the debug port! We're ignoring missed or incorrect responses.
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        probe
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .ok();
        probe.flush().ok();

        // If all goes well, we lost the debug port. Thanks, boot ROM. Let's bring it back.
        //
        // The ARM communication interface knows how to re-initialize the debug port.
        // Re-initializing the core(s) is on us.
        let ap = probe.fully_qualified_address();
        let interface = probe.get_arm_debug_interface()?;
        interface.reinitialize()?;

        assert!(debug_base.is_none());
        self.debug_core_start(interface, &ap, core_type, None, None)?;

        // Are we back?
        self.wait_for_enable(probe, Duration::from_millis(300))?;

        // We're back. Halt the core so we can establish the reset context.
        self.halt(probe, true)?;

        // When we reset into the boot ROM, it checks why we reset. If the boot ROM observes that
        // we reset due to SYSRESETREQ, it spins at a known address. Are we spinning there?
        self.check_pc(probe, Self::BOOT_ROM_SPIN_ADDRESS)?;

        // Why does the boot ROM spin? It wants us to set up the reset context! (And it wanted
        // to give us a chance to re-establish debugging after it took it away from us.)
        //
        // We assume that the user wants reset into the program they store within FlexSPI. We
        // emulate the behaviors of the boot ROM here: find the reset handler, and prepare the
        // CM7 to run that reset handler. It's convenient that the boot ROM prepares the FlexSPI
        // controller...
        //
        // But that's not always true: if you change your boot fuses, your board's boot pins, etc.
        // then the boot ROM respects that configuration. It might not initialize the FlexSPI
        // controller, and we won't be able to find the reset handler. We're not sure what to do
        // here, so we'll keep the CM7 in the boot ROM.
        //
        // (A generous tool might inspect the boot fuses to figure out what the next step would
        // be. Maybe it could invoke more boot ROM APIs to put us into the next stage. Sorry,
        // we're not yet a generous tool.)
        if let Some(pc) = self.find_flexspi_image_reset_handler(probe)? {
            self.write_core_reg(probe, PC, pc)?
        } else {
            tracing::warn!(
                "Could not find a valid reset handler in FlexSPI! Keeping the CM7 in the boot ROM."
            );
        }

        debug_cache.restore(probe)?;

        // We're halted in order to establish the reset context. Did the user want us to stay halted?
        if !self.simulate_reset_catch.load(Ordering::Relaxed) {
            self.halt(probe, false)?;
        }

        Ok(())
    }
}

/// Cache the debug state of the MCU.
///
/// Some targets will lose this state once they execute a system reset. For
/// targets that know this will happen, we can restore the context after
/// the reset occurs.
///
/// There's probably more we could cache, but this is a good enough starting
/// point for 1170 testing.
///
/// The FPB assumes the v2 architecture revision, and it only cares about
/// control and comparator registers. (No caching of any CoreSight IDs.)
/// A portable implementation may need to specialize this for the FPB revision
/// of the chip.
struct DebugCache {
    fp_ctrl: FpCtrl,
    fp_comps: Vec<FpRev2CompX>,
}

impl DebugCache {
    /// Produce a debug cache from the target.
    fn from_target(probe: &mut dyn ArmMemoryInterface) -> Result<Self, ArmError> {
        let fp_ctrl = FpCtrl(probe.read_word_32(FpCtrl::get_mmio_address())?);

        Ok(Self {
            fp_ctrl,
            fp_comps: (0..fp_ctrl.num_code())
                .map(|base_address| -> Result<FpRev2CompX, ArmError> {
                    let address = FpRev2CompX::get_mmio_address_from_base(base_address as u64 * 4)?;
                    let fp_comp = probe.read_word_32(address)?;
                    Ok(FpRev2CompX(fp_comp))
                })
                .collect::<Result<Vec<_>, _>>()?,
        })
    }

    /// Put this cached debug state back into the target.
    fn restore(mut self, probe: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        self.fp_ctrl.set_key(true);
        probe.write_word_32(FpCtrl::get_mmio_address(), self.fp_ctrl.into())?;

        for (base, fp_comp) in self.fp_comps.into_iter().enumerate() {
            probe.write_word_32(
                FpRev2CompX::get_mmio_address_from_base(base as u64 * 4)?,
                fp_comp.into(),
            )?;
        }

        Ok(())
    }
}

/// Debug sequences for the S32K3xx family.
///
/// This is a port of the debug sequences in NXP's `S32K3xx_DFP` CMSIS pack. The
/// family routes debug control through two non-memory access ports:
///
/// * `SDA_AP` (AP 7): the secure debug access port. Its `DBGENCTRL` register
///   gates all core debugging, and its `SDAAPRSTCTRL` register holds or
///   releases the cores when connecting under reset.
/// * `MDM_AP` (AP 6): the debug/miscellaneous control port. Its `MDMAPCTL`
///   register triggers functional resets and enables core register access
///   while a core is held in reset.
///
/// The system SRAM and the TCMs are ECC protected and must be initialized with
/// 64-bit writes before narrower accesses are reliable, which is required
/// before probe-rs can load a flash loader into SRAM. Like the CMSIS pack,
/// this sequence initializes them with a DMA transfer. Unlike the pack (which
/// initializes on every debug connection), this is done after a reset with a
/// reset catch armed — i.e. the reset-and-halt performed before flashing or
/// downloading to RAM. A plain attach never touches memory or clocks, so
/// attaching to a running system is non-invasive, and a plain reset leaves the
/// RAM initialization to the application's startup code, preserving retained
/// standby-RAM data.
#[derive(Debug)]
pub struct S32K3xx {
    /// MC_ME partitions 0..=1 exist on every family member, but partition 2
    /// only exists on parts larger than the S32K312.
    has_clock_partition2: bool,
    /// Bytes of standby/system SRAM starting at [`Self::SRAM_BASE`].
    sram_size: u32,
    /// Bytes of DTCM starting at 0x2000_0000, initialized through the
    /// [`Self::DTCM_BACKDOOR`] alias.
    dtcm_size: u32,
    /// Set when connecting under reset; selects the `DebugFromFirstInstruction`
    /// path of the pack's `DebugDeviceUnlock` sequence.
    connect_under_reset: AtomicBool,
}

impl S32K3xx {
    /// Secure debug access port.
    const SDA_AP: u8 = 7;
    /// SDA_AP debug enable control register.
    const SDA_AP_DBGENCTRL: u64 = 0x80;
    /// Enable global and CM7_0/CM7_1 debugging (GDBGEN, CM7_0_DBGEN, CM7_1_DBGEN, ...).
    const SDA_AP_DBGENCTRL_ENABLE_M7: u32 = 0x3000_00F0;
    /// SDA_AP reset release control register.
    const SDA_AP_RSTCTRL: u64 = 0x90;
    /// SDAAPRSTCTRL.RSTRELTLCM70: release CM7_0 from reset.
    const SDA_AP_RSTCTRL_RELEASE_CM7_0: u32 = 1 << 25;
    /// SDAAPRSTCTRL.RSTRELTLCM71: release CM7_1 from reset.
    const SDA_AP_RSTCTRL_RELEASE_CM7_1: u32 = 1 << 26;

    /// Debug/miscellaneous control access port.
    const MDM_AP: u8 = 6;
    /// MDM_AP control register (MDMAPCTL).
    const MDM_AP_CTL: u64 = 0x04;
    /// Keep RSTRELCM7/RSTRELTLn asserted.
    const MDM_AP_CTL_RSTREL: u32 = 0x0040_0000;
    /// RSTRELCM7/RSTRELTLn plus CMnDBGREQ.
    const MDM_AP_CTL_RSTREL_DBGREQ: u32 = 0x0040_0B00;
    /// RSTRELCM7/RSTRELTLn, CMnDBGREQ and SYSFUNCRST.
    const MDM_AP_CTL_RSTREL_DBGREQ_FUNCRST: u32 = 0x0040_0B20;
    /// CM7_0_CORE_ACCESS/CM7_1_CORE_ACCESS: allow core register access while
    /// the cores are held in reset.
    const MDM_AP_CTL_CORE_ACCESS: u32 = 0x0043_0000;

    /// Mode entry module, used to enable the peripheral clocks.
    const MC_ME: u64 = 0x402D_C000;

    /// DMAMUX_0 CHCFG0 register.
    const DMAMUX0_CHCFG0: u64 = 0x4028_0003;
    /// eDMA channel 0 base (CH0_CSR at offset 0, TCD words at 0x20..=0x3C).
    const EDMA_TCD0: u64 = 0x4021_0000;

    /// Base address of the system SRAM.
    const SRAM_BASE: u64 = 0x2040_0000;
    /// Base address of the DTCM.
    const DTCM_BASE: u64 = 0x2000_0000;
    /// The DTCM is only visible to the DMA through this backdoor alias.
    const DTCM_BACKDOOR: u32 = 0x2100_0000;

    /// Create a sequence handle for an S32K3xx chip, deriving the RAM regions
    /// that need ECC initialization from the target's memory map.
    pub fn create(chip: &Chip) -> Arc<dyn ArmDebugSequence> {
        // S32K310..S32K312 only have MC_ME partitions 0 and 1.
        let has_clock_partition2 = !["S32K310", "S32K311", "S32K312"]
            .iter()
            .any(|smaller| chip.name.starts_with(smaller));

        let mut dtcm_size = 0;
        let mut sram_ranges = Vec::new();
        for region in &chip.memory_map {
            let range = region.address_range();
            if range.start == Self::DTCM_BASE {
                dtcm_size = (range.end - range.start) as u32;
            } else if range.start >= Self::SRAM_BASE {
                sram_ranges.push(range);
            }
        }

        // The SRAM_n regions are contiguous, but the map may split them.
        sram_ranges.sort_by_key(|range| range.start);
        let mut sram_end = Self::SRAM_BASE;
        for range in sram_ranges {
            if range.start <= sram_end {
                sram_end = sram_end.max(range.end);
            }
        }

        Arc::new(Self {
            has_clock_partition2,
            sram_size: (sram_end - Self::SRAM_BASE) as u32,
            dtcm_size,
            connect_under_reset: AtomicBool::new(false),
        })
    }

    fn sda_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v1_with_dp(dp, Self::SDA_AP)
    }

    fn mdm_ap(dp: DpAddress) -> FullyQualifiedApAddress {
        FullyQualifiedApAddress::v1_with_dp(dp, Self::MDM_AP)
    }

    /// Enable M7 core debugging. This is the pack's `EnableM7Debug` sequence.
    fn enable_m7_debug(
        &self,
        interface: &mut dyn ArmDebugInterface,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        interface.write_raw_ap_register(
            &Self::sda_ap(dp),
            Self::SDA_AP_DBGENCTRL,
            Self::SDA_AP_DBGENCTRL_ENABLE_M7,
        )?;
        interface.flush()
    }

    /// Enable debugging and initialize the ECC RAMs. This is the pack's
    /// `DebugEnablement` sequence. Only used when connecting under reset,
    /// where the core is freshly released from reset and halted by the reset
    /// catch, so the memory and clock state can't belong to running code.
    fn debug_enablement(
        &self,
        interface: &mut dyn ArmDebugInterface,
        memory_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        self.enable_m7_debug(interface, memory_ap.dp())?;

        let mut memory = interface.memory_interface(memory_ap)?;
        self.enable_peripheral_clocks(&mut *memory)?;
        self.ram_initialize(&mut *memory)
    }

    /// Enable the peripheral clocks through the mode entry module. This is the
    /// pack's `EnablePeripheralClocks` sequence.
    fn enable_peripheral_clocks(
        &self,
        memory: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        tracing::debug!("Enabling peripheral clocks");

        let mut enable_partition = |partition: u64, cofb_clken: &[u32]| -> Result<(), ArmError> {
            let prtn = Self::MC_ME + 0x100 + partition * 0x200;
            for (block, &clken) in cofb_clken.iter().enumerate() {
                if clken != 0 {
                    // PRTNn_COFBm_CLKEN: enable the clock for the given blocks.
                    memory.write_word_32(prtn + 0x30 + block as u64 * 4, clken)?;
                }
            }
            // PRTNn_PCONF: enable clock to IPs.
            memory.write_word_32(prtn, 1)?;
            // PRTNn_PUPD: trigger the hardware process.
            memory.write_word_32(prtn + 4, 1)?;
            // MC_ME_CTL_KEY: start the hardware process.
            memory.write_word_32(Self::MC_ME, 0x5AF0)?;
            memory.write_word_32(Self::MC_ME, 0xA50F)?;
            Ok(())
        };

        enable_partition(0, &[0, 0x0000_F7DF])?;
        enable_partition(1, &[0xB1E0_FFF8, 0x812A_A407, 0xBBF3_FE7E, 0x0000_0141])?;
        if self.has_clock_partition2 {
            enable_partition(2, &[0x29FF_FFF0, 0xC489_87F9])?;
        }

        memory.flush()
    }

    /// Initialize the ECC RAMs via DMA. This is the pack's `RAMInitialize`
    /// sequence.
    fn ram_initialize(&self, memory: &mut dyn ArmMemoryInterface) -> Result<(), ArmError> {
        if self.sram_size != 0 {
            tracing::debug!("Initializing {} bytes of SRAM via DMA", self.sram_size);
            self.dma_fill(memory, Self::SRAM_BASE as u32, self.sram_size)?;
        }
        if self.dtcm_size != 0 {
            tracing::debug!("Initializing {} bytes of DTCM via DMA", self.dtcm_size);
            self.dma_fill(memory, Self::DTCM_BACKDOOR, self.dtcm_size)?;
        }
        Ok(())
    }

    /// Initialize `len` bytes of ECC RAM at `dest` with a single-major-loop
    /// DMA transfer that repeatedly reads the first flash doubleword.
    fn dma_fill(
        &self,
        memory: &mut dyn ArmMemoryInterface,
        dest: u32,
        len: u32,
    ) -> Result<(), ArmError> {
        const TCD: u64 = S32K3xx::EDMA_TCD0;
        const CH0_CSR_DONE: u32 = 1 << 30;

        // Enable DMA CH0 (DMAMUX_0 register CHCFG0).
        memory.write_word_8(Self::DMAMUX0_CHCFG0, 0x80)?;
        // Clear CH0_CSR.DONE from any earlier transfer (write-1-to-clear).
        memory.write_word_32(TCD, CH0_CSR_DONE)?;
        // TCD0_SADDR: read the program flash base over and over (SOFF = 0).
        memory.write_word_32(TCD + 0x20, 0x0040_0000)?;
        // TCD0_SOFF/TCD0_ATTR: source offset 0, 64-bit transfers.
        memory.write_word_32(TCD + 0x24, 0x0303_0000)?;
        // TCD0_NBYTES_MLOFFNO: transfer the whole region in one major iteration.
        memory.write_word_32(TCD + 0x28, len)?;
        // TCD0_SLAST_SDA: no source address adjustment.
        memory.write_word_32(TCD + 0x2C, 0)?;
        // TCD0_DADDR
        memory.write_word_32(TCD + 0x30, dest)?;
        // TCD0_DOFF/TCD0_CITER_ELINKNO: destination offset 8, major iteration count 1.
        memory.write_word_32(TCD + 0x34, 0x0001_0008)?;
        // TCD0_DLAST_SGA: rewind the destination address.
        memory.write_word_32(TCD + 0x38, len.wrapping_neg())?;
        // TCD0_CSR: start the transfer.
        memory.write_word_32(TCD + 0x3C, 1)?;
        memory.flush()?;

        // Poll CH0_CSR.DONE until the transfer finishes. The DONE flag is
        // sticky, unlike the ACTIVE flag the CMSIS pack polls, which may not
        // be set yet on the first poll.
        let start = Instant::now();
        while memory.read_word_32(TCD)? & CH0_CSR_DONE == 0 {
            if start.elapsed() > Duration::from_secs(1) {
                tracing::warn!("Timed out waiting for the RAM initialization DMA transfer");
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(1));
        }

        Ok(())
    }

    /// Release the nRESET pin and wait for the target to leave reset. This is
    /// the pack's `ResetHardwareDeassert_Default` sequence (and matches the
    /// default `reset_hardware_deassert`).
    fn release_reset_pin(&self, probe: &mut dyn ArmDebugInterface) -> Result<(), ArmError> {
        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let n_reset = n_reset.0 as u32;

        let can_read_pins = probe.swj_pins(n_reset, n_reset, 0)? != 0xffff_ffff;

        if can_read_pins {
            let start = Instant::now();
            loop {
                if Pins(probe.swj_pins(n_reset, n_reset, 0)? as u8).nreset() {
                    return Ok(());
                }
                if start.elapsed() >= Duration::from_secs(1) {
                    return Err(ArmError::Timeout);
                }
                thread::sleep(Duration::from_millis(100));
            }
        } else {
            thread::sleep(Duration::from_millis(100));
            Ok(())
        }
    }
}

impl ArmDebugSequence for S32K3xx {
    fn reset_hardware_assert(&self, interface: &mut dyn DapProbe) -> Result<(), ArmError> {
        // This is only called when connecting under reset; remember it so that
        // debug_device_unlock and reset_hardware_deassert can take the
        // connect-under-reset paths of the pack's sequences.
        self.connect_under_reset.store(true, Ordering::Relaxed);

        let mut n_reset = Pins(0);
        n_reset.set_nreset(true);
        let _ = interface.swj_pins(0, n_reset.0 as u32, 0)?;

        Ok(())
    }

    fn debug_device_unlock(
        &self,
        interface: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
        _permissions: &crate::Permissions,
    ) -> Result<(), ArmError> {
        let dp = default_ap.dp();

        if self.connect_under_reset.load(Ordering::Relaxed) {
            // The pack's `DebugFromFirstInstruction` sequence: keep the cores
            // in reset but make them debuggable, then release the nRESET pin.
            // The cores are individually released in reset_hardware_deassert,
            // after the reset catch is in place.
            self.enable_m7_debug(interface, dp)?;
            interface.write_raw_ap_register(
                &Self::mdm_ap(dp),
                Self::MDM_AP_CTL,
                Self::MDM_AP_CTL_CORE_ACCESS,
            )?;
            interface.write_raw_ap_register(&Self::sda_ap(dp), Self::SDA_AP_RSTCTRL, 0)?;
            interface.flush()?;

            self.release_reset_pin(interface)
        } else {
            // Unlike the pack's `DebugEnablement` sequence, only enable
            // debugging here: the clock and ECC RAM initialization would
            // destroy the memory and clock state of a running system. The
            // RAMs are initialized in reset_system when a reset catch is
            // armed, which covers flashing and RAM downloads.
            self.enable_m7_debug(interface, default_ap.dp())
        }
    }

    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        // Only called when connecting under reset. The nRESET pin was already
        // released in debug_device_unlock; release the connected core from
        // reset and enable debug. Like the pack (`__errorcontrol = 1`), ignore
        // errors from the reset release and debug enablement.
        let release = if default_ap.ap_v1()? == 5 {
            Self::SDA_AP_RSTCTRL_RELEASE_CM7_1
        } else {
            Self::SDA_AP_RSTCTRL_RELEASE_CM7_0
        };

        let result = probe
            .write_raw_ap_register(
                &Self::sda_ap(default_ap.dp()),
                Self::SDA_AP_RSTCTRL,
                release,
            )
            .and_then(|_| probe.flush());
        if let Err(error) = result {
            tracing::warn!("Ignoring error while releasing the core from reset: {error}");
        }

        if let Err(error) = self.debug_enablement(probe, default_ap) {
            tracing::warn!("Ignoring error during debug enablement: {error}");
        }

        self.connect_under_reset.store(false, Ordering::Relaxed);

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: probe_rs_target::CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // If a reset catch is armed, the core will halt before executing the
        // first instruction, and we initialize the ECC RAMs afterwards so that
        // code can be loaded into SRAM — this is the reset-and-halt performed
        // before flashing. Without a catch the application boots and performs
        // its own RAM initialization, like on a power-on, and touching the
        // RAM here would race the running code.
        let demcr = Demcr(interface.read_word_32(Demcr::get_mmio_address())?);

        // The pack's `ResetSystem` sequence issues a functional reset through
        // the MDM_AP (`FunctionalReset`) instead of using SYSRESETREQ. The
        // debug domain (including the reset catch in DEMCR) survives it.
        tracing::debug!("Initiating functional reset");

        let dp = interface.fully_qualified_address().dp();
        let mdm_ap = Self::mdm_ap(dp);
        let probe = interface.get_arm_debug_interface()?;

        probe.write_raw_ap_register(&mdm_ap, Self::MDM_AP_CTL, Self::MDM_AP_CTL_RSTREL_DBGREQ)?;
        probe.write_raw_ap_register(
            &mdm_ap,
            Self::MDM_AP_CTL,
            Self::MDM_AP_CTL_RSTREL_DBGREQ_FUNCRST,
        )?;
        probe.write_raw_ap_register(&mdm_ap, Self::MDM_AP_CTL, Self::MDM_AP_CTL_RSTREL_DBGREQ)?;
        probe.write_raw_ap_register(&mdm_ap, Self::MDM_AP_CTL, Self::MDM_AP_CTL_RSTREL)?;
        probe.flush()?;

        sequences::cortex_m_wait_for_reset(interface)?;

        if demcr.vc_corereset() {
            // The functional reset gated the peripheral clocks again, so
            // re-enable them before using the DMA.
            self.enable_peripheral_clocks(interface)?;
            self.ram_initialize(interface)?;
        }

        Ok(())
    }
}
