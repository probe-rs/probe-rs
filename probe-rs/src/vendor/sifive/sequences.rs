//! Debug sequences for SiFive RISC-V targets.
//!
//! The SiFive FU740 uses RISC-V Debug Spec 0.13.2.  This file is reserved
//! for chip-specific extensions that will be added in follow-up commits.

use std::sync::Arc;

use crate::architecture::riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence};

/// Returns the default debug sequence for SiFive chips.
pub fn sifive_sequence() -> Arc<dyn RiscvDebugSequence> {
    DefaultRiscvSequence::create()
}
