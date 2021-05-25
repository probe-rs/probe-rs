#![allow(dead_code)] // TODO remove this

use std::{
    collections::{BTreeMap, HashSet},
    convert::TryInto,
    env,
    ops::Deref,
};

use anyhow::{anyhow, bail};
use arrayref::array_ref;
use defmt_decoder::Table;
use object::{read::File as ElfFile, Object, ObjectSection, ObjectSymbol, SymbolSection};

use crate::VectorTable;

pub(crate) fn notmain() -> anyhow::Result<i32> {
    // - parse CL arguments
    // - parse ELF -> grouped into `ProcessedElf` struct
    //   -> RAM region
    //   -> location of RTT buffer
    //   -> vector table
    // - extra defmt table from ELF
    // - filter & connect to probe & configure
    // - flash the chip (optionally)
    // - write stack overflow canary in RAM
    // - set breakpoint
    // - start target program
    // - when paused, set RTT in blocking mode
    // - set breakpoint in HardFault handler
    // - resume target program
    // while !signal_received {
    //   - read RTT data
    //   - decode defmt logs from RTT data
    //   - print defmt logs
    //   - if core.is_halted() break
    // }
    // - if signal_received, halt the core
    // - [core is halted at this point]
    // - stack overflow check = check canary in RAM region
    // - print backtrace
    // - reset halt device to put peripherals in known state
    // - print exit reason

    todo!()
}

struct BacktraceInput {
    probe: (),
    // .debug_frame section
    debug_frame: (),
    // used for addr2line in frame symbolication
    elf: (),
}

pub(crate) struct ProcessedElf<'file> {
    // original ELF (object crate)
    elf: ElfFile<'file>,
    // name of functions in program after linking
    // extracted from `.text` section
    pub(crate) live_functions: HashSet<&'file str>,
    // // extracted using `defmt` crate
    // map(index: usize) -> defmt frame
    pub(crate) defmt_table: Option<Table>,
    pub(crate) defmt_locations: Option<BTreeMap<u64, defmt_decoder::Location>>,
    // // extracted from `for` loop over symbols
    // target_program_uses_heap: (),
    // rtt_buffer_address: (),
    // address_of_main_function: (),

    // // currently extracted via `for` loop over sections
    // debug_frame: (),                // gimli one (not bytes)
    // vector_table: (),               // processed one (not bytes)
    // highest_ram_address_in_use: (), // used for stack canary
}

impl<'file> ProcessedElf<'file> {
    pub(crate) fn from_elf(elf_bytes: &'file [u8]) -> Result<Self, anyhow::Error> {
        let elf = ElfFile::parse(elf_bytes)?;

        let live_functions = extract_live_functions(&elf)?;

        let (defmt_table, defmt_locations) = extract_defmt_info(elf_bytes)?;

        Ok(Self {
            defmt_table,
            defmt_locations,
            elf,
            live_functions,
        })
    }
    //     fn symbol_map(&self) -> SymbolMap {
    //         self.elf.symbol_map()
    //     }
}

// TODO remove this when we are done and don't need access to the internal elf anymore
impl<'elf> Deref for ProcessedElf<'elf> {
    type Target = ElfFile<'elf>;

    fn deref(&self) -> &ElfFile<'elf> {
        &self.elf
    }
}

fn extract_defmt_info(
    elf_bytes: &[u8],
) -> anyhow::Result<(
    Option<Table>,
    Option<BTreeMap<u64, defmt_decoder::Location>>,
)> {
    let defmt_table = match env::var("PROBE_RUN_IGNORE_VERSION").as_deref() {
        Ok("true") | Ok("1") => defmt_decoder::Table::parse_ignore_version(elf_bytes)?,
        _ => defmt_decoder::Table::parse(elf_bytes)?,
    };

    let mut defmt_locations = None;

    if let Some(table) = defmt_table.as_ref() {
        let tmp = table.get_locations(elf_bytes)?;

        if !table.is_empty() && tmp.is_empty() {
            log::warn!("insufficient DWARF info; compile your program with `debug = 2` to enable location info");
        } else if table.indices().all(|idx| tmp.contains_key(&(idx as u64))) {
            defmt_locations = Some(tmp);
        } else {
            log::warn!("(BUG) location info is incomplete; it will be omitted from the output");
        }
    }

    Ok((defmt_table, defmt_locations))
}

fn extract_live_functions<'file>(elf: &ElfFile<'file>) -> anyhow::Result<HashSet<&'file str>> {
    let text = elf
        .section_by_name(".text")
        .map(|section| section.index())
        .ok_or_else(|| {
            anyhow!(
                "`.text` section is missing, please make sure that the linker script was passed \
                to the linker (check `.cargo/config.toml` and the `RUSTFLAGS` variable)"
            )
        })?;

    let live_functions = elf
        .symbols()
        .filter_map(|sym| {
            if sym.section() == SymbolSection::Section(text) {
                Some(sym.name())
            } else {
                None
            }
        })
        .collect::<Result<HashSet<_>, _>>()?;
    Ok(live_functions)
}

pub(crate) fn extract_vector_table(elf: &ElfFile) -> anyhow::Result<VectorTable> {
    let section = elf
        .section_by_name(".vector_table")
        .ok_or_else(|| anyhow!("`.vector_table` section is missing"))?;

    let start = section.address().try_into()?;
    let size = section.size();

    if size % 4 != 0 || start % 4 != 0 {
        // we could support unaligned sections but let's not do that now
        bail!("section `.vector_table` is not 4-byte aligned");
    }

    let bytes = section.data()?;
    let mut words = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(*array_ref!(chunk, 0, 4)));

    if let (Some(initial_stack_pointer), Some(reset), Some(_third), Some(hard_fault)) =
        (words.next(), words.next(), words.next(), words.next())
    {
        Ok(VectorTable {
            location: start,
            initial_stack_pointer,
            reset,
            hard_fault,
        })
    } else {
        Err(anyhow!(
            "vector table section is too short. (has length: {} - should be at least 16)",
            bytes.len()
        ))
    }
}

struct DataFromProbeRsRegistry {
    ram_region_that_contains_stack: (),
}

// obtained via probe-rs?
// struct DataFromRunningTarget {}

// fn parse_cl_arguments() -> ClArguments {

// }
