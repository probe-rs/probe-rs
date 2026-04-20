//! SiFive vendor support.

use probe_rs_target::Chip;

use crate::{
    config::{DebugSequence, Registry},
    error::Error,
    vendor::Vendor,
};

pub mod sequences;

/// SiFive vendor.
#[derive(docsplay::Display)]
pub struct Sifive;

impl Vendor for Sifive {
    fn try_create_debug_sequence(&self, _chip: &Chip) -> Option<DebugSequence> {
        // SiFive chips share the default RISC-V debug sequence for now.
        // Chip-specific sequences will be added in a follow-up commit.
        None
    }

    fn try_detect_riscv_chip(
        &self,
        _registry: &Registry,
        _probe: &mut crate::architecture::riscv::communication_interface::RiscvCommunicationInterface,
        idcode: u32,
    ) -> Result<Option<String>, Error> {
        // FU740-C000 JTAG IDCODE: 0x20000913
        // Version=2, Part=0, Manufacturer=0x489 (SiFive, JEP106 bank 10 id 0x09)
        if idcode == 0x2000_0913 {
            tracing::info!(
                "SifiveVendor: detected FU740-C000 via IDCODE {:#010x}",
                idcode
            );
            return Ok(Some("FU740-C000".to_string()));
        }

        Ok(None)
    }
}
