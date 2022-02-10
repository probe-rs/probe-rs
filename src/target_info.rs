use std::{
    convert::TryInto,
    ops::{Range, RangeInclusive},
};

use object::{Object, ObjectSection as _};
use probe_rs::config::{MemoryRegion, RamRegion};

use crate::elf::Elf;

pub(crate) struct TargetInfo {
    pub(crate) probe_target: probe_rs::Target,
    /// RAM region that contains the call stack
    pub(crate) active_ram_region: Option<RamRegion>,
    pub(crate) stack_info: Option<StackInfo>,
}

pub(crate) struct StackInfo {
    /// Valid values of the stack pointer (that don't collide with other data).
    pub(crate) range: RangeInclusive<u32>,
    pub(crate) data_below_stack: bool,
}

impl TargetInfo {
    pub(crate) fn new(chip: &str, elf: &Elf) -> anyhow::Result<Self> {
        let probe_target = probe_rs::config::get_target_by_name(chip)?;
        let active_ram_region =
            extract_active_ram_region(&probe_target, elf.vector_table.initial_stack_pointer);
        let stack_info = active_ram_region
            .as_ref()
            .and_then(|ram_region| extract_stack_info(elf, &ram_region.range));

        Ok(Self {
            probe_target,
            active_ram_region,
            stack_info,
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

fn extract_stack_info(elf: &Elf, ram_range: &Range<u32>) -> Option<StackInfo> {
    // How does it work?
    // - the upper end of the stack is the initial SP, minus one
    // - the lower end of the stack is the highest address any section in the elf file uses, plus one

    let initial_stack_pointer = elf.vector_table.initial_stack_pointer;

    // SP points one past the end of the stack.
    let stack_end = initial_stack_pointer - 1;
    let mut stack_start = ram_range.start;

    for section in elf.sections() {
        let size: u32 = section.size().try_into().expect("expected 32-bit ELF");
        if size == 0 {
            continue;
        }

        let lowest_address: u32 = section.address().try_into().expect("expected 32-bit ELF");
        let highest_address = lowest_address + size - 1;
        let section_range = lowest_address..=highest_address;
        let name = section.name().unwrap_or("<unknown>");

        if ram_range.contains(section_range.end()) {
            log::debug!("section `{}` is in RAM at {:#010X?}", name, section_range);

            if section_range.contains(&stack_end) {
                log::debug!(
                    "initial SP is in section `{}`, cannot determine valid stack range",
                    name
                );
                return None;
            } else if section_range.contains(&stack_start) {
                stack_start = section_range.end() + 1;
            }
        }
    }

    let stack_range = stack_start..=stack_end;
    log::debug!("valid SP range: {:#010X?}", stack_range);
    Some(StackInfo {
        data_below_stack: *stack_range.start() > ram_range.start,
        range: stack_range,
    })
}
