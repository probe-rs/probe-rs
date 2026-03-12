//! Debug sequences for Nuclei RISC-V targets.
//!
//! The Nuclei UX600 uses standard RISC-V Debug Spec 0.13 with no special
//! initialization requirements, so we simply use the default RISC-V sequence.
//! This file is reserved for future chip-specific extensions.

use std::sync::Arc;

use crate::architecture::riscv::sequences::{DefaultRiscvSequence, RiscvDebugSequence};

/// Returns the default debug sequence for Nuclei chips.
pub fn nuclei_sequence() -> Arc<dyn RiscvDebugSequence> {
    DefaultRiscvSequence::create()
}
