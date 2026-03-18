//! Debug sequences for Zynq-7000 series SoCs

use crate::architecture::arm::{
    ArmError, FullyQualifiedApAddress, memory::ArmMemoryInterface, sequences::ArmDebugSequence,
};
use probe_rs_target::CoreType;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const SLCR_UNLOCK: u64 = 0xF800_0008;
const PSS_RST_CTRL: u64 = 0xF800_0200;
const REBOOT_STATUS: u64 = 0xF800_0258;

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
        let ap = FullyQualifiedApAddress::v1_with_default_dp(0);
        let mut mem_ap = interface.get_arm_debug_interface()?.memory_interface(&ap)?;

        mem_ap.write_word_32(SLCR_UNLOCK, 0xDF0D)?;
        let mut reg = mem_ap.read_word_32(REBOOT_STATUS)?;
        reg &= !(1 << 19);
        mem_ap.write_word_32(REBOOT_STATUS, reg)?;
        mem_ap.write_word_32(PSS_RST_CTRL, 1)?;

        std::thread::sleep(Duration::from_millis(100));

        // Poll for successful reset.
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(500) {
            match mem_ap.read_word_32(REBOOT_STATUS) {
                Ok(reg) if (reg >> 19) & 1 == 1 => {
                    tracing::debug!("Reset complete.");
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
