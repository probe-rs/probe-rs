//! Debug sequences for SiFive RISC-V targets.

use std::sync::{Arc, Mutex};

use std::time::Duration;

use crate::architecture::riscv::communication_interface::{
    AccessRegisterCommand, MemoryAccessMethod, RiscvBusAccess, RiscvCommunicationInterface,
};
use crate::architecture::riscv::sequences::RiscvDebugSequence;
use crate::core::RegisterId;
use crate::memory::MemoryInterface;

/// QSPI0 controller base address on the FU740-C000.
///
/// The onboard IS25WP256D SPI NOR flash is connected to QSPI0.
/// Its memory-mapped (XIP) window starts at 0x20000000.
const QSPI0_BASE: u64 = 0x1004_0000;

/// Offset of the Flash-Controller register (`fctrl`) within the QSPI block.
///
/// Bit 0: 1 = enable memory-mapped (XIP) flash access.
const QSPI_FCTRL: u64 = 0x60;

/// Bit 0 of `fctrl`: set to enable XIP, clear to disable.
const FCTRL_EN: u32 = 1;

/// Debug sequence for SiFive FU740-C000 (HiFive Unmatched).
///
/// # Program-buffer bootstrap (`dcsr.ebreakm`)
///
/// The FU740 Debug Module (Debug Spec 0.13.2) returns `cmderr=2` (not
/// supported) for abstract CSR access, but implements abstract GPR access
/// natively.  Program buffer execution requires `dcsr.ebreakm=1` so that
/// the `ebreak` at the end of the buffer enters debug mode; without it the
/// ebreak causes an M-mode exception.
///
/// `on_connect` sets `ebreakm=1` (and `prv=M`) using a minimal bootstrap:
///
/// 1. Write mask `0x8003` to x9 (s1) via abstract GPR command — this does
///    not use the program buffer.
/// 2. Execute progbuf: `csrrs x0, dcsr, x9` — atomically ORs the mask into
///    DCSR before the subsequent `ebreak` runs, so the ebreak correctly
///    enters debug mode.
///
/// # Program-buffer memory access
///
/// The FU740 System Bus (SB) is absent (sbversion=0), so all memory
/// accesses go through the program buffer.
///
/// # QSPI0 XIP re-enable
///
/// Linux's SPI NOR driver disables XIP mode (clears `fctrl` bit 0) when it
/// takes ownership of the QSPI0 controller.  On connect this sequence saves
/// the current `fctrl` value and sets bit 0, re-enabling the XIP window so
/// that probe-rs can read from 0x20000000 via program-buffer loads.
#[derive(Debug)]
pub struct SifiveSequence {
    /// Saved `fctrl` value, kept for documentation and potential future restore.
    saved_fctrl: Mutex<Option<u32>>,
}

impl SifiveSequence {
    /// Creates the SiFive FU740 debug sequence.
    pub fn create() -> Arc<dyn RiscvDebugSequence> {
        Arc::new(Self {
            saved_fctrl: Mutex::new(None),
        })
    }

    /// Bootstrap `dcsr.ebreakm=1` and `dcsr.prv=M` via program buffer.
    ///
    /// The FU740 DM does not support abstract CSR access (cmderr=2 for any
    /// CSR regno).  Abstract GPR access IS supported natively.  This method
    /// uses that distinction to set the two critical DCSR bits before any
    /// program-buffer memory operation is attempted.
    fn bootstrap_ebreakm(interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        // S1 = x9, abstract register address 0x1009.
        // 0x8003 = ebreakm (bit 15) | prv=M (bits 1:0 = 0b11).
        const S1_REGNO: RegisterId = RegisterId(0x1009);
        const DCSR_EBREAKM_PRV_M: u32 = (1 << 15) | 0x3;

        // csrrs x0, dcsr(0x7b0), x9  — OR mask into DCSR, write old value to x0 (discarded).
        // Encoding: [CSR:12][rs1:5][funct3:3][rd:5][opcode:7]
        //           [0x7b0][01001][010][00000][1110011] = 0x7B04A073
        const CSRRS_X0_DCSR_X9: u32 = 0x7B04A073;

        // Step 1: Write the mask to GPR x9 (s1) via abstract GPR command.
        // Abstract GPR access is implemented natively by the FU740 DM and
        // does NOT use the program buffer, so ebreakm=0 is not a problem.
        interface
            .abstract_cmd_register_write(S1_REGNO, DCSR_EBREAKM_PRV_M)
            .map_err(crate::Error::Riscv)?;

        // Step 2: Set up the program buffer with the csrrs instruction.
        // schedule_setup_program_buffer appends ebreak automatically.
        interface
            .schedule_setup_program_buffer(&[CSRRS_X0_DCSR_X9])
            .map_err(crate::Error::Riscv)?;

        // Step 3: Execute the program buffer (no data transfer, postexec=true).
        // The csrrs instruction sets ebreakm=1 atomically *before* ebreak runs,
        // so the ebreak correctly enters debug mode.
        let mut cmd = AccessRegisterCommand(0);
        cmd.set_cmd_type(0);
        cmd.set_aarsize(RiscvBusAccess::A32); // ignored when transfer=false
        cmd.set_postexec(true);
        cmd.set_transfer(false);
        interface
            .execute_abstract_command(cmd.0)
            .map_err(crate::Error::Riscv)?;

        tracing::debug!("SifiveSequence: dcsr.ebreakm=1 and prv=M set via progbuf bootstrap");
        Ok(())
    }
}

impl RiscvDebugSequence for SifiveSequence {
    /// Override reset_system_and_halt to avoid ndmreset on the FU740.
    ///
    /// The FU740's ndmreset resets ALL harts (including the U74 cores running
    /// Linux) and can leave the debug module in an inconsistent state.
    /// Instead we simply halt hart 0 (which is already halted or in OpenSBI's
    /// WFI loop) and re-bootstrap ebreakm.  The flash algorithm's Init
    /// function takes care of disabling XIP and configuring the SPI controller.
    fn reset_system_and_halt(
        &self,
        interface: &mut RiscvCommunicationInterface,
        timeout: Duration,
    ) -> Result<(), crate::Error> {
        // Safety: clear stale abstractauto and cmderr from previous sessions.
        interface.clear_abstractauto();

        // Perform hart reset to return to a clean state.  This is
        // necessary because a previous flash algorithm session may have
        // left the hart's PC in the middle of algorithm code that no
        // longer exists in RAM.
        interface.reset_hart_and_halt(timeout)?;

        // The reset cleared dcsr.ebreakm.  Re-bootstrap it so that the
        // flash algorithm's return ebreak enters debug mode.
        if let Err(e) = Self::bootstrap_ebreakm(interface) {
            tracing::warn!(
                "SifiveSequence::reset_system_and_halt: bootstrap_ebreakm failed: {:?}",
                e
            );
        }

        tracing::debug!("SifiveSequence: reset complete, dcsr.ebreakm=1 re-established");
        Ok(())
    }

    fn on_connect(&self, interface: &mut RiscvCommunicationInterface) -> Result<(), crate::Error> {
        // Safety: clear any stale abstractauto left by a previous session.
        // If autoexec was enabled and the session crashed, DATA0 reads during
        // bootstrap would trigger stale abstract commands and cause exceptions.
        interface.clear_abstractauto();

        // Step 1: Force program buffer for all access widths.
        // The FU740 has no System Bus (sbversion=0); all memory access must
        // go through the program buffer.
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

        // Step 2: Bootstrap dcsr.ebreakm=1 and dcsr.prv=M for hart 0 (S7/E51).
        //
        // Hart 0 is the only configured core for flashing; it is permanently
        // parked in OpenSBI's M-mode WFI loop.  Program buffer execution
        // requires ebreakm=1 so that the ebreak at the end enters debug mode
        // instead of causing an M-mode exception (cmderr=4 / HaltResume).
        if let Err(e) = Self::bootstrap_ebreakm(interface) {
            tracing::warn!(
                "SifiveSequence: bootstrap_ebreakm failed (will try to continue): {:?}",
                e
            );
        }

        // Step 3: Re-enable QSPI0 XIP mode so that the flash window at
        // 0x20000000 is readable via program-buffer load instructions.
        //
        // Linux's SPI NOR driver clears fctrl[0] when it takes over QSPI0.
        let fctrl_addr = QSPI0_BASE + QSPI_FCTRL;
        let current_fctrl = interface.read_word_32(fctrl_addr)?;

        tracing::debug!(
            "SifiveSequence: QSPI0 fctrl @ {:#010x} = {:#010x}",
            fctrl_addr,
            current_fctrl
        );

        if let Ok(mut saved) = self.saved_fctrl.lock() {
            *saved = Some(current_fctrl);
        }

        if current_fctrl & FCTRL_EN == 0 {
            tracing::info!(
                "SifiveSequence: QSPI0 XIP disabled (fctrl={:#010x}), re-enabling for flash access",
                current_fctrl
            );
            interface.write_word_32(fctrl_addr, current_fctrl | FCTRL_EN)?;
        } else {
            tracing::debug!("SifiveSequence: QSPI0 XIP already enabled");
        }

        Ok(())
    }
}
