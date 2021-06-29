use object::{Object, ObjectSection as _};
use probe_rs::config::{MemoryRegion, RamRegion};
use std::convert::TryInto;

use crate::elf::Elf;

pub(crate) struct TargetInfo {
    pub(crate) probe_target: probe_rs::Target,
    /// RAM region that contains the call stack
    pub(crate) active_ram_region: Option<RamRegion>,
    /// Only `Some` if static variables are located in the `active_ram_region`
    pub(crate) highest_static_var_address: Option<u32>,
}

impl TargetInfo {
    pub(crate) fn new(chip: &str, elf: &Elf) -> anyhow::Result<Self> {
        let probe_target = probe_rs::config::get_target_by_name(chip)?;
        let active_ram_region =
            extract_active_ram_region(&probe_target, elf.vector_table.initial_stack_pointer);
        let highest_static_var_address =
            extract_highest_static_var_address(elf, active_ram_region.as_ref());

        Ok(Self {
            probe_target,
            active_ram_region,
            highest_static_var_address,
        })
    }
}

fn extract_active_ram_region(
    target: &probe_rs::Target,
    initial_stack_pointer: u32,
) -> Option<RamRegion> {
    target
        .memory_map
        .iter()
        .find_map(|region| match region {
            MemoryRegion::Ram(ram_region) => {
                // NOTE stack is full descending; meaning the stack pointer can be
                // `ORIGIN(RAM) + LENGTH(RAM)`
                let inclusive_range = ram_region.range.start..=ram_region.range.end;
                if inclusive_range.contains(&initial_stack_pointer) {
                    log::debug!(
                        "RAM region: 0x{:08X}-0x{:08X}",
                        ram_region.range.start,
                        ram_region.range.end - 1
                    );
                    Some(ram_region)
                } else {
                    None
                }
            }
            _ => None,
        })
        .cloned()
}

fn extract_highest_static_var_address(
    elf: &object::read::File,
    active_ram_region: Option<&RamRegion>,
) -> Option<u32> {
    let active_ram_region = active_ram_region?;

    elf.sections()
        .filter_map(|section| {
            let size = section.size();
            if size == 0 {
                return None;
            }

            let lowest_address = section.address();
            let highest_address = (lowest_address + size - 1)
                .try_into()
                .expect("expected 32-bit ELF");

            if active_ram_region.range.contains(&highest_address) {
                log::debug!(
                    "section `{}` is in RAM at {:#010X} ..= {:#010X}",
                    section.name().unwrap_or("<unknown>"),
                    lowest_address,
                    highest_address,
                );

                Some(highest_address)
            } else {
                None
            }
        })
        .max()
}
