//! Sequences for NXP MCX chips.

use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bitfield::BitMut;
use debugmailbox::{DMCSW, DMREQUEST};
use probe_rs_target::CoreType;

use crate::{
    MemoryMappedRegister,
    architecture::arm::{
        ArmError, ArmProbeInterface, DapAccess, FullyQualifiedApAddress, Pins,
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

    fn is_variant<'a, V>(&self, v: V) -> bool
    where
        V: IntoIterator<Item = &'a str>,
    {
        v.into_iter().any(|s| self.variant.starts_with(s))
    }

    async fn is_ap_enabled(
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
            .read_raw_ap_register(mem_ap, CSW::ADDRESS)
            .await?
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

    async fn enable_debug_mailbox(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<bool, ArmError> {
        use crate::architecture::arm::{ap::IDR, dp::DPIDR};

        tracing::info!("enable debug mailbox");

        let ap = self.debug_mailbox_ap(dp)?;

        // Read APIDR
        let apidr: IDR = interface
            .read_raw_ap_register(&ap, IDR::ADDRESS)
            .await?
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
            .read_raw_dp_register(dp, DPIDR::ADDRESS)
            .await?
            .try_into()?;
        tracing::info!("DPIDR: {:?}", dpidr);

        tracing::info!("active DebugMailbox");
        interface
            .write_raw_ap_register(&ap, DMCSW::ADDRESS, 0x0000_0021)
            .await?;
        thread::sleep(Duration::from_millis(30));
        interface.read_raw_ap_register(&ap, 0x0).await?;
        interface.flush().await?;

        tracing::info!("DebugMailbox command: start debug session");
        interface
            .write_raw_ap_register(&ap, DMREQUEST::ADDRESS, 0x0000_0007)
            .await?;
        thread::sleep(Duration::from_millis(30));
        interface.read_raw_ap_register(&ap, 0x0).await?;
        interface.flush().await?;

        Ok(true)
    }

    async fn wait_for_stop_after_reset(
        &self,
        interface: &mut dyn ArmMemoryInterface,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Dhcsr;

        tracing::info!("wait for stop after reset");

        // Give bootloader time to do what it needs to do
        thread::sleep(Duration::from_millis(100));

        let ap = interface.fully_qualified_address();
        let dp = ap.dp();
        let start = Instant::now();
        while self.is_ap_enabled(interface.get_dap_access()?, &ap).await?
            && start.elapsed() < Duration::from_millis(300)
        {}
        self.enable_debug_mailbox(interface.get_dap_access()?, dp)
            .await?;

        // Halt the core in case it didn't stop at a breakpoint
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        interface
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())
            .await?;
        interface.flush().await?;

        // Clear watch point
        interface.write_word_32(0xE000_1020, 0x0).await?;
        interface.write_word_32(0xE000_1028, 0x0).await?;
        interface.write_word_32(0xE000_1030, 0x0).await?;
        interface.write_word_32(0xE000_1038, 0x0).await?;
        interface.flush().await?;

        // Clear XPSR to avoid undefined instruction fault caused by IT/ICI
        interface.write_word_32(0xE000_EDF8, 0x0100_0000).await?;
        interface.write_word_32(0xE000_EDF4, 0x0001_0010).await?;
        interface.flush().await?;

        // Set MSPLIM to 0
        interface.write_word_32(0xE000_EDF8, 0x0000_0000).await?;
        interface.write_word_32(0xE000_EDF4, 0x0001_001C).await?;
        interface.flush().await?;

        Ok(())
    }
}

#[async_trait::async_trait(?Send)]
impl ArmDebugSequence for MCX {
    async fn debug_port_start(
        &self,
        interface: &mut dyn DapAccess,
        dp: DpAddress,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::dp::{Abort, Ctrl, SelectV1};

        tracing::info!("debug port start");

        // Clear WDATAERR, STICKYORUN, STICKYCMP, STICKYERR
        let mut abort = Abort(0);
        abort.set_wderrclr(true);
        abort.set_orunerrclr(true);
        abort.set_stkcmpclr(true);
        abort.set_stkerrclr(true);
        interface.write_dp_register(dp, abort).await?;

        // Switch to DP Register Bank 0
        interface.write_dp_register(dp, SelectV1(0)).await?;

        // Read DP CTRL/STAT Register and check if CSYSPWRUPACK and CDBGPWRUPACK bits are set
        let ctrl: Ctrl = interface.read_dp_register(dp).await?;
        let powered_down = !ctrl.csyspwrupack() || !ctrl.cdbgpwrupack();

        if !powered_down {
            return Ok(());
        }

        // Request Debug/System Power-Up
        let mut ctrl = Ctrl(0);
        ctrl.set_csyspwrupreq(true);
        ctrl.set_cdbgpwrupreq(true);
        interface.write_dp_register(dp, ctrl).await?;

        // Wait for Power-Up request to be acknowledged
        let start = Instant::now();
        loop {
            let ctrl: Ctrl = interface.read_dp_register(dp).await?;
            if ctrl.csyspwrupack() && ctrl.cdbgpwrupack() {
                break;
            }
            if start.elapsed() > Duration::from_millis(1000) {
                return Err(ArmError::Timeout);
            }
        }

        if let Some(protocol) = interface.try_dap_probe().and_then(|f| f.active_protocol()) {
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
                    interface.write_dp_register(dp, ctrl).await?;
                }
                WireProtocol::Swd => {
                    // Init AP Transfer Mode, Transaction Counter, and
                    // Lane Mask (Normal Transfer Mode, Include  all Byte Lanes)
                    let mut ctrl = Ctrl(0);
                    ctrl.set_csyspwrupreq(true);
                    ctrl.set_cdbgpwrupreq(true);
                    ctrl.set_mask_lane(0b1111);
                    interface.write_dp_register(dp, ctrl).await?;

                    // Clear WDATAERR, STICKYORUN, STICKYCMP, and STICKYERR bits
                    // of CTRL/STAT Register by write to ABORT register
                    let mut abort = Abort(0);
                    abort.set_wderrclr(true);
                    abort.set_orunerrclr(true);
                    abort.set_stkcmpclr(true);
                    abort.set_stkerrclr(true);
                    interface.write_dp_register(dp, abort).await?;
                }
            }

            let ap = FullyQualifiedApAddress::v1_with_dp(dp, 0);
            if !self.is_ap_enabled(interface, &ap).await? {
                self.enable_debug_mailbox(interface, dp).await?;
            }
        }

        Ok(())
    }

    async fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::core::armv8m::{Aircr, Demcr, Dhcsr};

        tracing::info!("reset system");

        // Halt the core
        let mut dhcsr = Dhcsr(0);
        dhcsr.enable_write();
        dhcsr.set_c_halt(true);
        dhcsr.set_c_debugen(true);
        interface
            .write_word_32(Dhcsr::get_mmio_address(), dhcsr.into())
            .await?;
        interface.flush().await?;

        // Clear VECTOR CATCH and set TRCENA
        let mut demcr: Demcr = interface
            .read_word_32(Demcr::get_mmio_address())
            .await?
            .into();
        demcr.set_trcena(true);
        interface
            .write_word_32(Demcr::get_mmio_address(), demcr.into())
            .await?;
        interface.flush().await?;

        // Set watch point
        if self.is_variant(Self::VARIANT_A0) || self.is_variant(Self::VARIANT_A1) {
            interface.write_word_32(0xE000_1020, 0x4009_1036).await?;
            interface.write_word_32(0xE000_1028, 0xF000_0412).await?;
            interface.write_word_32(0xE000_1030, 0x4009_1040).await?;
            interface.write_word_32(0xE000_1038, 0xF000_0403).await?;
        } else {
            tracing::warn!("unknwon variant, try to set watch point");
            interface.write_word_32(0xE000_1020, 0x0000_0000).await?;
            interface.write_word_32(0xE000_1028, 0x0000_0412).await?;
            interface.write_word_32(0xE000_1030, 0x000F_FFFF).await?;
            interface.write_word_32(0xE000_1038, 0xF000_0403).await?;
        }
        interface.flush().await?;

        // Execute SYSRESETREQ via AIRCR
        let mut aircr = Aircr(0);
        aircr.vectkey();
        aircr.set_sysresetreq(true);
        let _ = interface
            .write_word_32(Aircr::get_mmio_address(), aircr.into())
            .await;

        let _ = self.wait_for_stop_after_reset(interface).await;

        Ok(())
    }

    async fn reset_catch_set(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::{Demcr, Dhcsr};

        tracing::info!("reset catch set");

        let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address()).await?.into();
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
            .await?;
        core.flush().await?;

        let reset_vector = core.read_word_32(0x0000_0004).await?;

        // Breakpoint on user application reset vector
        if reset_vector != 0xFFFF_FFFF {
            core.write_word_32(0xE000_2008, reset_vector | 0x1).await?;
            core.write_word_32(0xE000_2000, 0x0000_0003).await?;
        }
        // Enable reset vector catch
        if reset_vector == 0xFFFF_FFFF {
            let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address()).await?.into();
            demcr.set_vc_corereset(true);
            core.write_word_32(Demcr::get_mmio_address(), demcr.into())
                .await?;
        }
        core.flush().await?;

        core.read_word_32(Dhcsr::get_mmio_address()).await?;

        Ok(())
    }

    async fn reset_catch_clear(
        &self,
        core: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        use crate::architecture::arm::armv8m::Demcr;

        tracing::info!("reset catch clear");

        core.write_word_32(0xE000_2008, 0x0000_0000).await?;
        core.write_word_32(0xE000_2000, 0x0000_0002).await?;

        let mut demcr: Demcr = core.read_word_32(Demcr::get_mmio_address()).await?.into();
        demcr.set_vc_corereset(false);
        core.write_word_32(Demcr::get_mmio_address(), demcr.into())
            .await?;

        Ok(())
    }

    async fn reset_hardware_deassert(
        &self,
        probe: &mut dyn ArmProbeInterface,
        _default_ap: &FullyQualifiedApAddress,
    ) -> Result<(), ArmError> {
        tracing::info!("reset hardware deassert");
        let n_reset = Pins(0x80).0 as u32;

        let can_read_pins = probe.swj_pins(0, n_reset, 0).await? != 0xFFFF_FFFF;

        thread::sleep(Duration::from_millis(50));

        let mut assert_n_reset = async || probe.swj_pins(n_reset, n_reset, 0).await;
        if can_read_pins {
            let start = Instant::now();
            let timeout_occured = || start.elapsed() > Duration::from_millis(1000);

            while assert_n_reset().await? & n_reset == 0 && !timeout_occured() {}
        } else {
            assert_n_reset().await?;
            thread::sleep(Duration::from_millis(100));
        }

        let ap = FullyQualifiedApAddress::v1_with_dp(probe.current_debug_port(), 0);
        let mut interface = probe.memory_interface(&ap).await?;
        self.wait_for_stop_after_reset(interface.as_mut()).await?;

        Ok(())
    }
}
