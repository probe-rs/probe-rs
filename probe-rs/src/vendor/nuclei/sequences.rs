//! Debug sequences for Nuclei RISC-V targets.

use std::sync::{Arc, Mutex};

use crate::architecture::riscv::communication_interface::{
    MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface,
};
use crate::architecture::riscv::sequences::RiscvDebugSequence;
use crate::memory::MemoryInterface;

/// NUSPI controller base address on Nuclei chips.
///
/// This is the standard address used in the Nuclei SDK and OpenOCD configuration
/// for EvalSoC-based targets (`xipnuspi_base = 0x10014000`).
const NUSPI_BASE: u64 = 0x1001_4000;

/// Offset of the Flash Controller register (`fctrl`) within the NUSPI block.
///
/// Bit 0 of this register enables memory-mapped (XIP) access to the SPI flash.
/// When a Linux kernel SPI driver takes ownership of the NUSPI controller it
/// clears this bit, which makes the XIP window at `0x20000000` return all-zeros.
const NUSPI_FCTRL: u64 = 0x60;

/// Bit 0 of `fctrl`: set to enable XIP (memory-mapped flash), clear to disable.
const FCTRL_EN: u32 = 1;

/// Debug sequence for Nuclei RISC-V chips.
///
/// Some Nuclei chips have a System Bus (SB) that only covers on-chip memories
/// such as boot ROM and ILM/DLM. Accesses to external regions like XIP SPI
/// flash (`0x20000000`) and DDR DRAM (`0x40000000`) go through peripheral
/// controllers that are not reachable by the debug System Bus. The SB returns
/// all-zeros for those addresses without asserting `sberror`, which would
/// silently produce incorrect reads (and fail to back up flash contents).
///
/// This sequence overrides the memory access defaults to use the program buffer
/// path for all widths, which executes actual load/store instructions on the
/// halted CPU and can reach every address the CPU can — including XIP flash and
/// DRAM.
///
/// # NUSPI XIP re-enable
///
/// When Linux boots it takes ownership of the NUSPI SPI flash controller and
/// disables XIP mode (clears `fctrl` bit 0). This makes reads to `0x20000000`
/// return all-zeros even via the program buffer. On connect this sequence
/// saves the current `fctrl` value and sets bit 0 to re-enable XIP.
///
/// Because the `RiscvDebugSequence` trait has no `on_disconnect` hook the
/// original value is stored but cannot be automatically restored on session
/// close. Linux's SPI driver will re-initialize the controller when the CPU
/// resumes, so this is safe for read-only flash backup sessions.
#[derive(Debug)]
pub struct NucleiSequence {
    /// Saved `fctrl` value from before we enabled XIP, for documentation and
    /// potential future restore when an `on_disconnect` hook is added.
    saved_fctrl: Mutex<Option<u32>>,
}

impl NucleiSequence {
    /// Creates the Nuclei debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            saved_fctrl: Mutex::new(None),
        })
    }
}

impl RiscvDebugSequence for NucleiSequence {
    /// After `init()` has auto-detected SB support, override all default memory
    /// access methods to use program buffer instead of system bus, then
    /// re-enable NUSPI XIP mode so that `0x20000000` is readable.
    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        // Force program buffer for all access widths; the SB on some Nuclei
        // chips cannot reach XIP flash or DDR and returns zeros without sberror.
        let config = interface.memory_access_config();
        for width in [
            RiscvBusAccess::A8,
            RiscvBusAccess::A16,
            RiscvBusAccess::A32,
            RiscvBusAccess::A64,
            RiscvBusAccess::A128,
        ] {
            config.set_default_method(width, MemoryAccessMethod::ProgramBuffer);
        }

        // Re-enable XIP mode on the NUSPI flash controller.
        //
        // When Linux is running its SPI driver clears fctrl[0], which disables
        // the memory-mapped XIP window at 0x20000000. We set it again here so
        // that CPU load instructions via the program buffer can read flash.
        //
        // The write goes through the program buffer path (sw instruction) so
        // it reaches the peripheral address space correctly.
        let fctrl_addr = NUSPI_BASE + NUSPI_FCTRL;
        let current_fctrl = interface.read_word_32(fctrl_addr)?;

        tracing::debug!(
            "NucleiSequence: NUSPI fctrl @ {:#010x} = {:#010x}",
            fctrl_addr,
            current_fctrl
        );

        // Save for documentation; cannot restore automatically (no on_disconnect hook).
        if let Ok(mut saved) = self.saved_fctrl.lock() {
            *saved = Some(current_fctrl);
        }

        if current_fctrl & FCTRL_EN == 0 {
            tracing::info!(
                "NucleiSequence: NUSPI XIP disabled (fctrl={:#010x}), enabling XIP for flash backup",
                current_fctrl
            );
            interface.write_word_32(fctrl_addr, current_fctrl | FCTRL_EN)?;
        } else {
            tracing::debug!("NucleiSequence: NUSPI XIP already enabled");
        }

        Ok(())
    }
}
