//! Nuclei vendor support for RISC-V cores.

use probe_rs_target::Chip;

use crate::{
    config::{DebugSequence, Registry},
    error::Error,
    vendor::Vendor,
};

pub mod sequences;

/// Nuclei Technology Corporation
#[derive(docsplay::Display)]
pub struct Nuclei;

impl Vendor for Nuclei {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        if chip.name.starts_with("Nuclei") {
            Some(DebugSequence::Riscv(sequences::NucleiSequence::create()))
        } else {
            None
        }
    }

    fn try_detect_riscv_chip(
        &self,
        _registry: &Registry,
        _probe: &mut crate::architecture::riscv::communication_interface::RiscvCommunicationInterface,
        _idcode: u32,
    ) -> Result<Option<String>, Error> {
        // Nuclei chips do not currently have a public JEP106 code, so auto-detection
        // is not implemented. The user must specify the target explicitly.
        Ok(None)
    }
}
