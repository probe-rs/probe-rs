//! Microchip vendor support.

use probe_rs_target::Chip;
use termtree::Tree;

use crate::{
    architecture::arm::{
        ap::MemoryAp,
        memory::{romtable::RomTable, ComponentId},
        ArmProbeInterface,
    },
    config::DebugSequence,
    vendor::{
        microchip::sequences::atsam::{AtSAM, DsuDid},
        Vendor,
    },
    Error,
};

pub mod sequences;

/// Microchip
#[derive(docsplay::Display)]
pub struct Microchip;

impl Vendor for Microchip {
    fn try_create_debug_sequence(&self, chip: &Chip) -> Option<DebugSequence> {
        let sequence = if chip.name.starts_with("ATSAMD1")
            || chip.name.starts_with("ATSAMD2")
            || chip.name.starts_with("ATSAMDA")
            || chip.name.starts_with("ATSAMD5")
            || chip.name.starts_with("ATSAME5")
        {
            DebugSequence::Arm(AtSAM::create())
        } else {
            return None;
        };

        Some(sequence)
    }

    fn parse_custom_rom_table(
        &self,
        interface: &mut dyn ArmProbeInterface,
        id: &ComponentId,
        _table: &RomTable,
        access_port: MemoryAp,
        tree: &mut Tree<String>,
    ) -> Result<(), Error> {
        let peripheral_id = id.peripheral_id();
        if peripheral_id.designer() == Some("Atmel") && peripheral_id.part() == 0xCD0 {
            // Read and parse the DID register
            let did = DsuDid(
                interface
                    .memory_interface(access_port)?
                    .read_word_32(DsuDid::ADDRESS)?,
            );

            tree.push(format!("Atmel device (DID = {:#010x})", did.0));
        }

        Ok(())
    }
}
