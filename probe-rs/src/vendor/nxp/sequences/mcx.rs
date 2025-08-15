//! Sequences for NXP MCX chips.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitfield::BitMut;
use debugmailbox::{DMCSW, DMREQUEST, DMRETURN};
use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmDebugInterface, ArmError, DapAccess, FullyQualifiedApAddress, Pins,
        ap::ApRegister,
        dp::{DpAccess, DpAddress, DpRegister},
        memory::ArmMemoryInterface,
        sequences::ArmDebugSequence,
    },
    probe::WireProtocol,
};

mod debugmailbox {
    use crate::architecture::arm::ap::define_ap_register;

    define_ap_register!(
        name: DMCSW,
        address: 0x00,
        fields: [
            ResynchReq: bool,
            ReqPending: bool,
            DbgOrErr: bool,
            AhbOrErr: bool,
            SoftReset: bool,
            ChipResetReq: bool,
        ],
        from: value => Ok(DMCSW {
            ResynchReq: ((value >> 31) & 0x01) != 0,
            ReqPending: ((value >> 30) & 0x01) != 0,
            DbgOrErr: ((value >> 29) & 0x01) != 0,
            AhbOrErr: ((value >> 28) & 0x01) != 0,
            SoftReset: ((value >> 27) & 0x01) != 0,
            ChipResetReq: ((value >> 26) & 0x01) != 0,
        }),
        to: value => (u32::from(value.ResynchReq) << 31)
        | (u32::from(value.ReqPending) << 30)
        | (u32::from(value.DbgOrErr) << 29)
        | (u32::from(value.AhbOrErr) << 28)
        | (u32::from(value.SoftReset) << 27)
        | (u32::from(value.ChipResetReq) << 26)
    );

    define_ap_register!(
        name: DMREQUEST,
        address: 0x04,
        fields: [
            Request: u32
        ],
        from: value => Ok(DMREQUEST {
            Request: value
        }),
        to: value => value.Request
    );

    define_ap_register!(
        name: DMRETURN,
        address: 0x08,
        fields: [
            Return: u32
        ],
        from: value => Ok(DMRETURN {
            Return: value
        }),
        to: value => value.Return
    );
}

/// Debug sequences for MCX family MCUs.
#[derive(Debug)]
pub struct MCX {
    variant: String, // part variant
}

impl MCX {
    /// Create a sequence handle for MCX MCUs.
    pub fn create(variant: String) -> Arc<dyn ArmDebugSequence> {
        Arc::new(Self { variant })
    }

    const VARIANT_A: [&str; 1] = ["MCXA"];
    const VARIANT_A0: [&str; 4] = ["MCXA153", "MCXA152", "MCXA143", "MCXA142"];
    const VARIANT_A1: [&str; 6] = [
        "MCXA156", "MCXA155", "MCXA154", "MCXA146", "MCXA145", "MCXA144",
    ];
    // const VARIANT_A2: [&str; 3] = ["MCXA16", "MCXA17", "MCXA27"];
    const VARIANT_N: [&str; 1] = ["MCXN"];
    const VARIANT_N0: [&str; 1] = ["MCXN947"];

    fn is_variant<'a, V>(&self, v: V) -> bool
    where
        V: IntoIterator<Item = &'a str>,
    {
        v.into_iter().any(|s| self.variant.starts_with(s))
    }

    fn is_ap_enabled(
        &self,
        interface: &mut dyn DapAccess,
        mem_ap: &FullyQualifiedApAddress,
    ) -> Result<bool, ArmError> {
        use crate::architecture::arm::ap::CSW;

        if mem_ap == &self.debug_mailbox_ap(mem_ap.dp())? {
            // DebugMailbox AP is always enabled
            return Ok(true);
        }
        let csw: CSW = interface
            .read_raw_ap_register(mem_ap, CSW::ADDRESS)?
            .try_into()?;
        Ok(csw.DeviceEn)
    }

    fn debug_mailbox_ap(&self, dp: DpAddress) -> Result<FullyQualifiedApAddress, ArmError> {
        if self.is_variant(Self::VARIANT_N) {
            Ok(FullyQualifiedApAddress::v1_with_dp(dp, 2))
        } else if self.is_variant(Self::VARIANT_A) {
            Ok(FullyQualifiedApAddress::v1_with_dp(dp, 1))
        } else {
            tracing::error!("unknown DebugMailbox AP");
            Err(ArmError::NotImplemented("unknown DebugMailbox AP"))
        }
    }

    fn enable_debug_mailbox(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<bool, ArmError> {
        use crate::architecture::arm::{ap::IDR, dp::DPIDR};

        tracing::info!("enable debug mailbox");

        let ap = self.debug_mailbox_ap(dp)?;

        // Read APIDR
        let apidr: IDR = interface
            .read_raw_ap_register(&ap, IDR::ADDRESS)?
            .try_into()?;
        tracing::info!("APIDR: {:?}", apidr);
        tracing::info!("APIDR: 0x{:08X}", u32::from(apidr));
        if u32::from(apidr) != 0x002A_0000 {
            // This AP is not DebugMailbox!
            tracing::error!("ap {:?} is not DebugMailbox!", ap);
            return Err(ArmError::WrongApType);
        }

        // Read DPIDR
        let dpidr: DPIDR = interface
            .read_raw_dp_register(dp, DPIDR::ADDRESS)?
            .try_into()?;
        tracing::info!("DPIDR: {:?}", dpidr);

        // Write RESYNCH_REQ + CHIP_RESET_REQ (0x21 = 0x20 | 0x01)
        interface.write_raw_ap_register(&ap, DMCSW::ADDRESS, 0x0000_0021)?;

        // Poll CSW register for zero return, indicating success
        let start = Instant::now();
        loop {
            let csw_val = interface.read_raw_ap_register(&ap, DMCSW::ADDRESS)?;
            if (csw_val & 0xFFFF) == 0 {
                break;
            }
            if start.elapsed() > Duration::from_millis(1000) {
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        }
        tracing::info!("RESYNC_REQ + CHIP_RESET_REQ: success");

        // Write START_DBG_SESSION to REQUEST register
        tracing::info!("DebugMailbox command: start debug session (0x07)");
        interface.write_raw_ap_register(&ap, DMREQUEST::ADDRESS, 0x0000_0007)?;

        // Poll RETURN register for zero return
        let start = Instant::now();
        loop {
            let return_val = interface.read_raw_ap_register(&ap, DMRETURN::ADDRESS)? & 0xFFFF;
            if return_val == 0 {
                break;
            }
            if start.elapsed() > Duration::from_millis(1000) {
                return Err(ArmError::Timeout);
            }
            thread::sleep(Duration::from_millis(10));
        }
        tracing::info!("DEBUG_SESSION_REQ: success");

        interface.flush()?;

        Ok(true)
    }

    fn configure_trace_clock(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        tracing::info!("configuring trace clock");

        if self.is_variant(Self::VARIANT_N0) {
            // MCXN947 specific register addresses
            const SYSCON_NS_BASE: u64 = 0x40000000;
            const TRACECLKSEL_ADDR: u64 = SYSCON_NS_BASE + 0x268;
            const TRACECLKDIV_ADDR: u64 = SYSCON_NS_BASE + 0x308;
            const AHBCLKCTRLSET0_ADDR: u64 = SYSCON_NS_BASE + 0x220;

            interface.write_word_32(TRACECLKSEL_ADDR, 0x0)?;
            interface.write_word_32(TRACECLKDIV_ADDR, 0x2)?;

            // Enable PORT0 clock for trace pins
            interface.write_word_32(AHBCLKCTRLSET0_ADDR, 1 << 13)?;
        } else if self.is_variant(Self::VARIANT_A0) {
            const SYSCON_NS_BASE: u64 = 0x40091000;
            const TRACECLKSEL_ADDR: u64 = SYSCON_NS_BASE + 0x138;
            const TRACECLKDIV_ADDR: u64 = SYSCON_NS_BASE + 0x13C;
            const AHBRESETSET0_ADDR: u64 = SYSCON_NS_BASE + 0x04;
            const AHBCLKCTRLSET0_ADDR: u64 = SYSCON_NS_BASE + 0x44;

            interface.write_word_32(TRACECLKSEL_ADDR, 0x0)?;

            // Read current TRACECLKDIV value, preserve divider but clear rest to enable
            let clkdiv = interface.read_word_32(TRACECLKDIV_ADDR)? & 0xFF;
            interface.write_word_32(TRACECLKDIV_ADDR, clkdiv)?;

            // Release Port0 from reset
            interface.write_word_32(AHBRESETSET0_ADDR, 1 << 29)?;
            // Enable Port0 clock
            interface.write_word_32(AHBCLKCTRLSET0_ADDR, 1 << 29)?;
        }

        interface.flush()?;
        Ok(())
    }

    fn wait_for_stop_after_reset(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Dhcsr;

        tracing::info!("wait for stop after reset");

        // Give bootloader time to do what it needs to do
        if self.is_variant(Self::VARIANT_N0) {
            thread::sleep(Duration::from_millis(1000));
        } else {
            thread::sleep(Duration::from_millis(100));
        }

        let ap = interface.fully_qualified_address();
        let dp = ap.dp();

        let start = Instant::now();
        let timeout = if self.is_variant(Self::VARIANT_N0) {
            Duration::from_millis(500)
        } else {
            Duration::from_millis(300)
        };

        while !self.is_ap_enabled(interface.get_arm_debug_interface()?, &ap)?
            && start.elapsed() < timeout
        {
            thread::sleep(Duration::from_millis(10));
        }

        // Try to enable debug mailbox if AP is still not enabled
        if !self.is_ap_enabled(interface.get_arm_debug_interface()?, &ap)? {
            self.enable_debug_mailbox(interface.get_arm_debug_interface()?, dp)?;
        }

        // Halt the core in case it didn't stop at a breakpoint
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        interface.flush()?;

        // Clear watch points
        interface.write_word_32(0xE000_1020, 0x0)?;
        interface.write_word_32(0xE000_1028, 0x0)?;
        interface.write_word_32(0xE000_1030, 0x0)?;
        interface.write_word_32(0xE000_1038, 0x0)?;
        interface.flush()?;

        // Clear XPSR to avoid undefined instruction fault caused by IT/ICI
        interface.write_word_32(0xE000_EDF8, 0x0100_0000)?;
        interface.write_word_32(0xE000_EDF4, 0x0001_0010)?;
        interface.flush()?;

        // Set MSPLIM to 0
        interface.write_word_32(0xE000_EDF8, 0x0000_0000)?;
        interface.write_word_32(0xE000_EDF4, 0x0001_001C)?;
        interface.flush()?;

        Ok(())
    }
}

impl ArmDebugSequence for MCX {
    fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::dp::{Abort, Ctrl, SelectV1};

        tracing::info!("debug port start for MCX variant: {}", self.variant);

        // Switch to DP Register Bank 0
        interface.write_dp_register(dp, SelectV1(0))?;

        // Clear WDATAERR, STICKYORUN, STICKYCMP, STICKYERR
        let mut abort = Abort(0);
        abort.set_wderrclr(true);
        abort.set_orunerrclr(true);
        abort.set_stkcmpclr(true);
        abort.set_stkerrclr(true);
        interface.write_dp_register(dp, abort)?;

        // Read DP CTRL/STAT Register and check if CSYSPWRUPACK and CDBGPWRUPACK bits are set
        let ctrl: Ctrl = interface.read_dp_register(dp)?;
        let powered_down = !ctrl.csyspwrupack() || !ctrl.cdbgpwrupack();

        if powered_down {
            // Request Debug/System Power-Up
            let mut ctrl = Ctrl(0);
            ctrl.set_csyspwrupreq(true);
            ctrl.set_cdbgpwrupreq(true);
            interface.write_dp_register(dp, ctrl)?;

            // Wait for Power-Up request to be acknowledged
            let start = Instant::now();
            let timeout = if self.is_variant(Self::VARIANT_N0) {
                Duration::from_millis(1000)
            } else {
                Duration::from_millis(500)
            };

            loop {
                let ctrl: Ctrl = interface.read_dp_register(dp)?;
                if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                    break;
                }
                if start.elapsed() > timeout {
                    return Err(ArmError::Timeout);
                }
                thread::sleep(Duration::from_millis(10));
            }
        }

        if let Some(protocol) = interface.try_dap_probe().and_then(|f| f.active_protocol()) {
            match protocol {
                WireProtocol::Jtag => {
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
                    let mut ctrl = Ctrl(0);
                    ctrl.set_csyspwrupreq(true);
                    ctrl.set_cdbgpwrupreq(true);
                    ctrl.set_mask_lane(0b1111);
                    interface.write_dp_register(dp, ctrl)?;

                    let mut abort = Abort(0);
                    abort.set_wderrclr(true);
                    abort.set_orunerrclr(true);
                    abort.set_stkcmpclr(true);
                    abort.set_stkerrclr(true);
                    interface.write_dp_register(dp, abort)?;
                }
            }
        }

        // Check if AP0 is disabled and enable debug mailbox if needed
        let ap = FullyQualifiedApAddress::v1_with_dp(dp, 0);
        if !self.is_ap_enabled(interface, &ap)? {
            self.enable_debug_mailbox(interface, dp)?;
        }

        Ok(())
    }

    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv8m::{Aircr, Demcr, Dhcsr};

        tracing::info!("reset system for MCX variant: {}", self.variant);

        // Halt the core
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        interface.write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())?;
        interface.flush()?;

        // Clear VECTOR CATCH and set TRCENA
        let mut demcr: Demcr = interface.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_trcena(true);
        interface.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        interface.flush()?;

        // Set watch points based on variant
        if self.is_variant(Self::VARIANT_A0) || self.is_variant(Self::VARIANT_A1) {
            interface.write_word_32(0xE000_1020, 0x4009_1036)?;
            interface.write_word_32(0xE000_1028, 0xF000_0412)?;
            interface.write_word_32(0xE000_1030, 0x4009_1040)?;
            interface.write_word_32(0xE000_1038, 0xF000_0403)?;
        } else if self.is_variant(Self::VARIANT_N0) {
            interface.write_word_32(0xE000_1020, 0x0000_0000)?;
            interface.write_word_32(0xE000_1028, 0x0000_0412)?;
            interface.write_word_32(0xE000_1030, 0x00FF_FFFF)?;
            interface.write_word_32(0xE000_1038, 0x0000_0403)?;

            interface.write_word_32(0xE000_1040, 0x8000_0000)?;
            interface.write_word_32(0xE000_1048, 0x0000_0412)?;
            interface.write_word_32(0xE000_1050, 0x8FFF_FFFF)?;
            interface.write_word_32(0xE000_1058, 0x0000_0403)?;

            // Reinit clock
            interface.write_word_32(0x4000_0220, 0x401)?;
            interface.write_word_32(0x4000_0140, 0x401)?;
            interface.write_word_32(0x4000_0220, 0xE000)?;
            interface.write_word_32(0x4000_0140, 0xE000)?;
        } else {
            tracing::warn!("unknown variant, using default watchpoint configuration");
            interface.write_word_32(0xE000_1020, 0x0000_0000)?;
            interface.write_word_32(0xE000_1028, 0x0000_0412)?;
            interface.write_word_32(0xE000_1030, 0x000F_FFFF)?;
            interface.write_word_32(0xE000_1038, 0xF000_0403)?;
        }
        interface.flush()?;

        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let _ = interface.write_word_32(Aircr::get_mmio_address(), aircr.into());

        let _ = self.configure_trace_clock(interface);

        let _ = self.wait_for_stop_after_reset(interface);

        Ok(())
    }

    fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::{Demcr, Dhcsr};

        tracing::info!("reset catch set for MCX variant: {}", self.variant);

        let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        core.flush()?;

        let reset_vector = if self.is_variant(Self::VARIANT_N0) {
            tracing::info!("reading reset vector via flash controller");

            // Flash controller addresses for MCXN947
            const FLASH_BASE: u64 = 0x40034000;
            const FLASH_STARTA: u64 = FLASH_BASE + 0x10;
            const FLASH_STOPA: u64 = FLASH_BASE + 0x14;
            const FLASH_DATAW0: u64 = FLASH_BASE + 0x80;
            const FLASH_CMD: u64 = FLASH_BASE;
            const FLASH_INT_CLR_STATUS: u64 = FLASH_BASE + 0xFE8;

            // Program Flash Word Start/Stop Address to 0x0
            core.write_word_32(FLASH_STARTA, 0x0)?;
            core.write_word_32(FLASH_STOPA, 0x0)?;

            // Clear data words
            for i in 0..8 {
                core.write_word_32(FLASH_DATAW0 + (i * 4), 0x0)?;
            }

            // Clear flash controller status
            core.write_word_32(FLASH_INT_CLR_STATUS, 0x0000_000F)?;

            // Read single flash word command
            core.write_word_32(FLASH_CMD, 0x0000_0003)?;

            core.flush()?;

            // Try to read reset vector via AHB
            let reset_vector = core.read_word_32(0x0000_0004)?;

            // Check if we need to use secure address space
            let enable_secure_check1 = core.read_word_32(0x4012_0FFC)? & 0xC;
            let enable_secure_check2 = core.read_word_32(0x4012_0FF8)? & 0xC;

            if enable_secure_check1 != 0x8 || enable_secure_check2 != 0x8 {
                tracing::info!("reading reset vector from secure address space");
                core.read_word_32(0x1000_0004)?
            } else {
                reset_vector
            }
        } else {
            core.read_word_32(0x0000_0004)?
        };

        // Breakpoint on user application reset vector
        if reset_vector != 0xFFFF_FFFF {
            tracing::info!("setting breakpoint on reset vector: 0x{:08X}", reset_vector);
            core.write_word_32(0xE000_2008, reset_vector | 0x1)?;
            core.write_word_32(0xE000_2000, 0x0000_0003)?;
        } else {
            // Enable reset vector catch
            tracing::info!("enabling reset vector catch");
            let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address())?.into();
            demcr.set_vc_corereset(true);
            core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;
        }
        core.flush()?;

        // Read DHCSR to clear potentially set DHCSR.S_RESET_ST bit
        core.read_word_32(Dhcsr::get_mmio_address())?;

        Ok(())
    }

    fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Demcr;

        tracing::info!("reset catch clear");

        // Clear FPB Comparators
        core.write_word_32(0xE000_2008, 0x0000_0000)?;

        // Disable FPB
        core.write_word_32(0xE000_2000, 0x0000_0002)?;

        // Clear reset vector catch
        let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address())?.into();
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())?;

        Ok(())
    }

    fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmDebugInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("reset hardware deassert for MCX variant: {}", self.variant);
        let n_reset = Pins(0x80).0 as u32;

        let can_read_pins = probe.swj_pins(0, n_reset, 0)? != 0xFFFF_FFFF;

        let reset_duration = if self.is_variant(Self::VARIANT_N0) {
            Duration::from_millis(100)
        } else {
            Duration::from_millis(50)
        };
        thread::sleep(reset_duration);

        let mut assert_n_reset = || probe.swj_pins(n_reset, n_reset, 0);
        if can_read_pins {
            let start = Instant::now();
            let timeout_occured = || start.elapsed() > Duration::from_millis(1000);

            while assert_n_reset()? & n_reset == 0 && !timeout_occured() {}
        } else {
            assert_n_reset()?;
            let recovery_time = if self.is_variant(Self::VARIANT_N0) {
                Duration::from_millis(200)
            } else {
                Duration::from_millis(100)
            };
            thread::sleep(recovery_time);
        }

        let ap = FullyQualifiedApAddress::v1_with_dp(probe.current_debug_port().unwrap(), 0);
        let mut interface = probe.memory_interface(&ap)?;
        self.wait_for_stop_after_reset(interface.as_mut())?;

        Ok(())
    }
}
