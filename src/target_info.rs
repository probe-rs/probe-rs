use std::{
    convert::TryInto,
    ops::{Range, RangeInclusive},
    path::Path,
};

use object::{Object, ObjectSection as _};
use probe_rs::{
    config::Core,
    config::{MemoryRegion, RamRegion},
    CoreType,
};

use crate::elf::Elf;

pub struct TargetInfo {
    pub probe_target: probe_rs::Target,
    /// RAM region that contains the call stack
    pub active_ram_region: Option<RamRegion>,
    pub stack_info: Option<StackInfo>,
}

pub struct StackInfo {
    /// Valid values of the stack pointer (that don't collide with other data).
    pub range: RangeInclusive<u32>,
    pub data_below_stack: bool,
}

impl TargetInfo {
    pub fn new(chip: &str, elf: &Elf) -> anyhow::Result<Self> {
        let probe_target = probe_rs::config::get_target_by_name(chip)?;
        check_processor_target_compatability(&probe_target.cores, elf.elf_path);

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

/// Check if the compilation target and processor fit and emit a warning if not.
fn check_processor_target_compatability(cores: &[Core], elf_path: &Path) {
    let target = elf_path.iter().find_map(|a| {
        let b = a.to_string_lossy();
        match b.starts_with("thumbv") {
            true => Some(b),
            false => None,
        }
    });
    let target = match target {
        Some(target) => target,
        None => return, // NOTE(return) If probe-run is not called through `cargo run` the elf_path
                        // might not contain the compilation target. In that case we return early.
    };

    // NOTE(indexing): There *must* always be at least one core.
    let core_type = cores[0].core_type;
    let matches = match core_type {
        CoreType::Armv6m => target == "thumbv6m-none-eabi",
        CoreType::Armv7m => target == "thumbv7m-none-eabi",
        CoreType::Armv7em => target == "thumbv7em-none-eabi" || target == "thumbv7em-none-eabihf",
        CoreType::Armv8m => {
            target == "thumbv8m.base-none-eabi"
                || target == "thumbv8m.main-none-eabi"
                || target == "thumbv8m.main-none-eabihf"
        }
        CoreType::Armv7a | CoreType::Armv8a => {
            log::warn!("Unsupported architecture ({core_type:?}");
            return;
        }
        // NOTE(return) Since we do not get any info about instruction
        // set support from probe-rs we do not know which compilation
        // targets fit.
        CoreType::Riscv => return,
    };

    if matches {
        return;
    }
    let recommendation = match core_type {
        CoreType::Armv6m => "must be 'thumbv6m-none-eabi'",
        CoreType::Armv7m => "should be 'thumbv7m-none-eabi'",
        CoreType::Armv7em => {
            "should be 'thumbv7em-none-eabi' (no FPU) or 'thumbv7em-none-eabihf' (with FPU)"
        }
        CoreType::Armv8m => {
            "should be 'thumbv8m.base-none-eabi' (M23), 'thumbv8m.main-none-eabi' (M33 no FPU), or 'thumbv8m.main-none-eabihf' (M33 with FPU)"
        }
        CoreType::Armv7a | CoreType::Armv8a => unreachable!(),
        CoreType::Riscv => unreachable!(),
    };
    log::warn!("Compilation target ({target}) and core type ({core_type:?}) do not match. Your compilation target {recommendation}.");
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
                if inclusive_range.contains(&initial_stack_pointer.into()) {
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

fn extract_stack_info(elf: &Elf, ram_range: &Range<u64>) -> Option<StackInfo> {
    // How does it work?
    // - the upper end of the stack is the initial SP, minus one
    // - the lower end of the stack is the highest address any section in the elf file uses, plus one

    let initial_stack_pointer = elf.vector_table.initial_stack_pointer;

    // SP points one word (4-byte) past the end of the stack.
    let mut stack_range =
        ram_range.start.try_into().unwrap_or(u32::MAX)..=initial_stack_pointer - 4;

    for section in elf.sections() {
        let size: u32 = section.size().try_into().expect("expected 32-bit ELF");
        if size == 0 {
            continue;
        }

        let lowest_address: u32 = section.address().try_into().expect("expected 32-bit ELF");
        let highest_address = lowest_address + size - 1;
        let section_range = lowest_address..=highest_address;
        let name = section.name().unwrap_or("<unknown>");

        if ram_range.contains(&(*section_range.end() as u64)) {
            log::debug!("section `{}` is in RAM at {:#010X?}", name, section_range);

            if section_range.contains(stack_range.end()) {
                log::debug!(
                    "initial SP is in section `{}`, cannot determine valid stack range",
                    name
                );
                return None;
            } else if stack_range.contains(section_range.end()) {
                stack_range = section_range.end() + 1..=*stack_range.end();
            }
        }
    }

    log::debug!("valid SP range: {:#010X?}", stack_range);
    Some(StackInfo {
        data_below_stack: *stack_range.start() as u64 > ram_range.start,
        range: stack_range,
    })
}
