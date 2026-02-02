//! Debug sequences for Zynq-7000 series SoCs

use crate::{
    MemoryMappedRegister as _,
    architecture::arm::{
        ArmError, FullyQualifiedApAddress, core::armv7a_debug_regs::Dbgdrcr,
        memory::ArmMemoryInterface, sequences::ArmDebugSequence,
    },
};
use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SLCR_UNLOCK: u64 = 0xF800_0008;
const PSS_RST_CTRL: u64 = 0xF800_0200;
const REBOOT_STATUS: u64 = 0xF800_0258;

/// Debug base address for core 0.
pub const CORE_0_DEBUG_BASE: u64 = 0x80090000;
/// Debug base address for core 1.
pub const CORE_1_DEBUG_BASE: u64 = 0x80092000;

/// Zynq-7000 SoCs have three APs
#[derive(Debug, Copy, Clone)]
#[repr(u8)]
pub enum AccessPort {
    /// MEM-AP that masters the main memory bus, can access all memory
    /// without CPU intervention, but is not on the CPU debug bus.
    /// Debug component access is via global memory-mapped address.
    /// Can't modify debug registers when software lock is active.
    SystemMemory = 0,
    ///MEM-AP on the APB-AP debug bus. Can only access debug peripherals.
    ///Uses a different debug base address, hence 0x80090000 below instead
    ///of the global address 0xF8890000 in the user guide.
    Debug = 1,
    ///JTAG-AP which can only be used to drive SRST from debug.
    Jtag = 2,
}

/// Xilinx Zynq-7000 series SoCs.
#[derive(Debug)]
pub struct X7Z {}

impl X7Z {
    /// Create a debug sequence for a Zynq 7000-series SoC.
    pub fn create() -> Arc<Self> {
        Arc::new(X7Z {})
    }
}

impl ArmDebugSequence for X7Z {
    fn reset_system(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        _core_type: CoreType,
        _debug_base: Option<u64>,
    ) -> Result<(), ArmError> {
        // Must implement custom reset as the DBGPRSR warm request bit doesn't do anything
        // and due to errata 55328 (Arm 799770) we also can't detect it.
        tracing::debug!("Triggering Zynq-7000 system reset via PSS_RST_CTRL");

        // Use AP 0 which directly connects to system memory.
        let ap = FullyQualifiedApAddress::v1_with_default_dp(AccessPort::SystemMemory as u8);
        let mut mem_ap = interface.get_arm_debug_interface()?.memory_interface(&ap)?;

        mem_ap.write_word_32(SLCR_UNLOCK, 0xDF0D)?;
        let mut reg = mem_ap.read_word_32(REBOOT_STATUS)?;
        reg &= !(1 << 19);
        mem_ap.write_word_32(REBOOT_STATUS, reg)?;
        // Ignore error here, they are expected for a reset.
        let _ = mem_ap.write_word_32(PSS_RST_CTRL, 1);

        std::thread::sleep(Duration::from_millis(100));

        // Poll for successful reset.
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(500) {
            match mem_ap.read_word_32(REBOOT_STATUS) {
                Ok(reg) if (reg >> 19) & 1 == 1 => {
                    tracing::debug!("Reset complete.");
                    // The reset vector catch is problematic for some system configurations. We
                    // might have to manually halt the cores after a system reset.
                    self.ensure_core_halted(mem_ap.as_mut(), CORE_0_DEBUG_BASE)?;
                    self.ensure_core_halted(mem_ap.as_mut(), CORE_1_DEBUG_BASE)?;
                    return Ok(());
                }
                _ => {
                    // Faults expected during reset. If they persist we'll time out.
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
        tracing::debug!("Timed out waiting for reset");
        Err(ArmError::Timeout)
    }
}

impl X7Z {
    fn ensure_core_halted(
        &self,
        interface: &mut dyn ArmMemoryInterface,
        debug_base: u64,
    ) -> Result<(), ArmError> {
        let ap = FullyQualifiedApAddress::v1_with_default_dp(AccessPort::Debug as u8);
        let mut debug_ap = interface.get_arm_debug_interface()?.memory_interface(&ap)?;
        if !crate::architecture::arm::armv7a::core_halted(debug_ap.as_mut(), debug_base)? {
            let address = Dbgdrcr::get_mmio_address_from_base(debug_base)?;
            let mut value = Dbgdrcr(0);
            value.set_hrq(true);
            debug_ap.write_word_32(address, value.into())?;
        }
        crate::architecture::arm::armv7a::wait_for_core_halted(
            debug_ap.as_mut(),
            debug_base,
            Duration::from_millis(100),
        )?;
        Ok(())
    }
}
