use crate::{architecture::arm::memory::romtable::RomTable, MemoryInterface};

use super::{ApAddress, ApV2Address, ArmCommunicationInterface};

struct RootMemoryAp {
    base_addr: u64
}


pub fn scan_rom_tables(
    _probe: &mut ArmCommunicationInterface<Initialized>,
    base_address: u64,
) -> Result<BTreeSet<ApAddress>, ArmError> {
    // build a root memory interface
    let rom_table = RomTable::try_parse(memory, base_address)?;
    for e in rom_table.entries() {
        // if e is a mem_ap
        //  add it to the set
        //  process its rom_table
    }
    todo!()
}

fn scan_rom_tables_internal(
    _probe: &mut ArmCommunicationInterface<Initialized>,
    _mem_aps: &mut BTreeSet<ApAddress>,
    _current_addr: Option<ApV2Address>,
) -> Result<(), ArmError> {
    todo!()
}
