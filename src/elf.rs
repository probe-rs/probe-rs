use std::{collections::HashSet, convert::TryInto, env, ops::Deref, path::Path};

use anyhow::{anyhow, bail};
use defmt_decoder::{Locations, Table};
use object::{
    read::File as ObjectFile, Object as _, ObjectSection as _, ObjectSymbol as _, SymbolSection,
};

use crate::cortexm;

pub struct Elf<'file> {
    elf: ObjectFile<'file>,
    symbols: Symbols,

    pub debug_frame: DebugFrame<'file>,
    pub defmt_locations: Option<Locations>,
    pub defmt_table: Option<Table>,
    pub elf_path: &'file Path,
    pub live_functions: HashSet<&'file str>,
    pub vector_table: cortexm::VectorTable,
}

impl<'file> Elf<'file> {
    pub fn parse(elf_bytes: &'file [u8], elf_path: &'file Path) -> Result<Self, anyhow::Error> {
        let elf = ObjectFile::parse(elf_bytes)?;

        let live_functions = extract_live_functions(&elf)?;

        let (defmt_table, defmt_locations) = extract_defmt_info(elf_bytes)?;
        let vector_table = extract_vector_table(&elf)?;
        log::debug!("vector table: {:x?}", vector_table);

        let debug_frame = extract_debug_frame(&elf)?;

        let symbols = extract_symbols(&elf)?;

        Ok(Self {
            elf,
            symbols,
            debug_frame,
            defmt_locations,
            defmt_table,
            elf_path,
            live_functions,
            vector_table,
        })
    }

    pub fn main_fn_address(&self) -> u32 {
        self.symbols.main_fn_address
    }

    pub fn program_uses_heap(&self) -> bool {
        self.symbols.program_uses_heap
    }

    pub fn rtt_buffer_address(&self) -> Option<u32> {
        self.symbols.rtt_buffer_address
    }
}

impl<'elf> Deref for Elf<'elf> {
    type Target = ObjectFile<'elf>;

    fn deref(&self) -> &ObjectFile<'elf> {
        &self.elf
    }
}

fn extract_live_functions<'file>(elf: &ObjectFile<'file>) -> anyhow::Result<HashSet<&'file str>> {
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
        .filter_map(|symbol| {
            if symbol.section() == SymbolSection::Section(text) {
                Some(symbol.name())
            } else {
                None
            }
        })
        .collect::<Result<HashSet<_>, _>>()?;

    Ok(live_functions)
}

fn extract_defmt_info(elf_bytes: &[u8]) -> anyhow::Result<(Option<Table>, Option<Locations>)> {
    let defmt_table = match env::var("PROBE_RUN_IGNORE_VERSION").as_deref() {
        Ok("true") | Ok("1") => defmt_decoder::Table::parse_ignore_version(elf_bytes)?,
        _ => defmt_decoder::Table::parse(elf_bytes)?,
    };

    let mut defmt_locations = None;

    if let Some(table) = defmt_table.as_ref() {
        let locations = table.get_locations(elf_bytes)?;

        if !table.is_empty() && locations.is_empty() {
            log::warn!("insufficient DWARF info; compile your program with `debug = 2` to enable location info");
        } else if table
            .indices()
            .all(|idx| locations.contains_key(&(idx as u64)))
        {
            defmt_locations = Some(locations);
        } else {
            log::warn!("(BUG) location info is incomplete; it will be omitted from the output");
        }
    }

    Ok((defmt_table, defmt_locations))
}

fn extract_vector_table(elf: &ObjectFile) -> anyhow::Result<cortexm::VectorTable> {
    let section = elf
        .section_by_name(".vector_table")
        .ok_or_else(|| anyhow!("`.vector_table` section is missing"))?;

    let start = section.address();
    let size = section.size();

    if size % 4 != 0 || start % 4 != 0 {
        bail!("section `.vector_table` is not 4-byte aligned");
    }

    let bytes = section.data()?;
    let mut words = bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()));

    if let (Some(initial_stack_pointer), Some(_reset), Some(_third), Some(hard_fault)) =
        (words.next(), words.next(), words.next(), words.next())
    {
        Ok(cortexm::VectorTable {
            initial_stack_pointer,
            hard_fault,
        })
    } else {
        Err(anyhow!(
            "vector table section is too short. (has length: {} - should be at least 16)",
            bytes.len()
        ))
    }
}

type DebugFrame<'file> = gimli::DebugFrame<gimli::EndianSlice<'file, cortexm::Endianness>>;

fn extract_debug_frame<'file>(elf: &ObjectFile<'file>) -> anyhow::Result<DebugFrame<'file>> {
    let bytes = elf
        .section_by_name(".debug_frame")
        .map(|section| section.data())
        .transpose()?
        .ok_or_else(|| anyhow!("`.debug_frame` section not found"))?;

    let mut debug_frame = gimli::DebugFrame::new(bytes, cortexm::ENDIANNESS);
    debug_frame.set_address_size(cortexm::ADDRESS_SIZE);
    Ok(debug_frame)
}

struct Symbols {
    rtt_buffer_address: Option<u32>,
    program_uses_heap: bool,
    main_fn_address: u32,
}

fn extract_symbols(elf: &ObjectFile) -> anyhow::Result<Symbols> {
    let mut rtt_buffer_address = None;
    let mut program_uses_heap = false;
    let mut main_fn_address = None;

    for symbol in elf.symbols() {
        let name = match symbol.name() {
            Ok(name) => name,
            Err(_) => continue,
        };

        let address = symbol.address().try_into().expect("expected 32-bit ELF");
        match name {
            "main" => main_fn_address = Some(cortexm::clear_thumb_bit(address)),
            "_SEGGER_RTT" => rtt_buffer_address = Some(address),
            "__rust_alloc" | "__rg_alloc" | "__rdl_alloc" | "malloc" if !program_uses_heap => {
                log::debug!("symbol `{}` indicates heap is in use", name);
                program_uses_heap = true;
            }
            _ => {}
        }
    }

    let main_function_address =
        main_fn_address.ok_or_else(|| anyhow!("`main` symbol not found"))?;

    Ok(Symbols {
        rtt_buffer_address,
        program_uses_heap,
        main_fn_address: main_function_address,
    })
}
