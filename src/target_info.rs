use object::{read::File as ElfFile, Object, ObjectSection};
use probe_rs::config::{MemoryRegion, RamRegion};
use std::convert::TryInto;

use crate::elf::ProcessedElf;

pub(crate) struct TargetInfo {
    pub(crate) target: probe_rs::Target,
    pub(crate) active_ram_region: Option<RamRegion>,
    pub(crate) highest_ram_addr_in_use: Option<u32>, // todo maybe merge
}

impl TargetInfo {
    pub(crate) fn new(chip: &str, elf: &ProcessedElf) -> anyhow::Result<Self> {
        let target = probe_rs::config::registry::get_target_by_name(chip)?;
        let active_ram_region =
            extract_active_ram_region(&target, elf.vector_table.initial_stack_pointer);
        let highest_ram_addr_in_use =
            extract_highest_ram_addr_in_use(elf, active_ram_region.as_ref())?;
        Ok(Self {
            target,
            active_ram_region,
            highest_ram_addr_in_use,
        })
    }
}

fn extract_active_ram_region(target: &probe_rs::Target, initial_sp: u32) -> Option<RamRegion> {
    target
        .memory_map
        .iter()
        .filter_map(|region| match region {
            MemoryRegion::Ram(region) => {
                // NOTE stack is full descending; meaning the stack pointer can be
                // `ORIGIN(RAM) + LENGTH(RAM)`
                let range = region.range.start..=region.range.end;
                if range.contains(&initial_sp) {
                    Some(region)
                } else {
                    None
                }
            }
            _ => None,
        })
        .next()
        .cloned()
}

fn extract_highest_ram_addr_in_use(
    elf: &ElfFile,
    active_ram_region: Option<&RamRegion>,
) -> anyhow::Result<Option<u32>> {
    let mut highest_ram_addr_in_use = None;
    for sect in elf.sections() {
        // If this section resides in RAM, track the highest RAM address in use.
        if let Some(ram) = active_ram_region {
            if sect.size() != 0 {
                let last_addr = sect.address() + sect.size() - 1;
                let last_addr = last_addr.try_into()?;
                if ram.range.contains(&last_addr) {
                    log::debug!(
                        "section `{}` is in RAM at 0x{:08X}-0x{:08X}",
                        sect.name().unwrap_or("<unknown>"),
                        sect.address(),
                        last_addr,
                    );
                    highest_ram_addr_in_use = highest_ram_addr_in_use.max(Some(last_addr));
                }
            }
        }
    }

    Ok(highest_ram_addr_in_use)
}
