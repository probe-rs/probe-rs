use goblin::{
    elf::program_header::PT_LOAD,
    elf64::section_header::{SHT_NOBITS, SHT_PROGBITS},
};
use probe_rs_target::MemoryRange;

use anyhow::{Result, anyhow};

const CODE_SECTION_KEY: (&str, u32) = ("PrgCode", SHT_PROGBITS);
const DATA_SECTION_KEY: (&str, u32) = ("PrgData", SHT_PROGBITS);
const BSS_SECTION_KEY: (&str, u32) = ("PrgData", SHT_NOBITS);
const GOT_SECTION_KEY: (&str, u32) = (".got", SHT_PROGBITS);
const GOT_PLT_SECTION_KEY: (&str, u32) = (".got.plt", SHT_PROGBITS);

/// List of "suspicious" section names
///
/// These sections are usually present in Rust/C binaries,
/// but should not be present in flash loader binaries.
///
/// If these are observed in the binary, we issue a warning.
const SUSPICIOUS_SECTION_NAMES: &[&str] = &[".text", ".rodata", ".data", ".sdata", ".bss", ".sbss"];

/// An ELF section of the flash algorithm ELF.
#[derive(Debug, Clone)]
pub(crate) struct Section {
    pub(crate) start: u32,
    pub(crate) length: u32,
    pub(crate) data: Vec<u8>,

    /// Load address for this section.
    ///
    /// For position independent code, this will not be used.
    pub(crate) load_address: u32,
}

/// A struct to hold all the binary sections of a flash algorithm ELF that go into flash.
#[derive(Debug, Clone)]
pub(crate) struct AlgorithmBinary {
    pub(crate) code_section: Section,
    pub(crate) static_base: u32,
    pub(crate) address_relocation_ranges: Vec<RelocationRange>,
    runtime_sections: Vec<Section>,
    runtime_start: u32,
    runtime_end: u32,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct RelocationRange {
    pub(crate) offset: u32,
    pub(crate) size: u32,
}

impl AlgorithmBinary {
    /// Extract a new flash algorithm binary blob from an ELF data blob.
    pub(crate) fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8]) -> Result<Self> {
        let mut code_section = None;
        let mut data_section = None;
        let mut bss_section = None;
        let mut got_section = None;
        let mut relocation_sections = Vec::new();
        let mut runtime_sections = Vec::new();

        let mut suspicious_sections = Vec::new();

        // Iterate all program headers and get sections.
        for ph in &elf.program_headers {
            // Only regard sections that contain at least one byte.
            // And are marked loadable (this filters out debug symbols).
            if ph.p_type == PT_LOAD && ph.p_memsz > 0 {
                let sector = ph.p_offset..ph.p_offset + ph.p_memsz;

                log::debug!("Program header: LOAD to VMA {:#010x}", ph.p_vaddr);

                // Scan all sectors if they contain any part of the sections found.
                for sh in &elf.section_headers {
                    let range = sh.sh_offset..sh.sh_offset + sh.sh_size;
                    if sector.contains_range(&range) {
                        // If we found a valid section, store its contents if any.
                        let data = if sh.sh_type == SHT_NOBITS {
                            Vec::new()
                        } else {
                            Vec::from(&buffer[sh.sh_offset as usize..][..sh.sh_size as usize])
                        };

                        let section = Some(Section {
                            start: sh.sh_addr as u32,
                            length: sh.sh_size as u32,
                            data,
                            load_address: (ph.p_vaddr + sh.sh_offset - ph.p_offset) as u32,
                        });

                        if let Some(section) = &section {
                            runtime_sections.push(section.clone());
                        }

                        // Make sure we store the section contents under the right name.
                        match (&elf.shdr_strtab[sh.sh_name], sh.sh_type) {
                            CODE_SECTION_KEY => code_section = section,
                            DATA_SECTION_KEY => data_section = section,
                            BSS_SECTION_KEY => bss_section = section,
                            GOT_SECTION_KEY => {
                                got_section = section.clone();
                                if let Some(section) = section {
                                    relocation_sections.push(section);
                                }
                            }
                            GOT_PLT_SECTION_KEY => {
                                if let Some(section) = section {
                                    relocation_sections.push(section);
                                }
                            }
                            (name, _section_type) => {
                                if SUSPICIOUS_SECTION_NAMES.contains(&name) {
                                    suspicious_sections.push(name);
                                }
                            }
                        }
                    }
                }
            }
        }

        if !suspicious_sections.is_empty() {
            log::warn!(
                "The ELF file contains some unexpected sections, which should not be part of a flash loader: "
            );

            for section in suspicious_sections {
                log::warn!("\t{section}");
            }

            log::warn!(
                "Code should be placed in the '{}' section, and data should be placed in the '{}' section.",
                CODE_SECTION_KEY.0,
                DATA_SECTION_KEY.0
            );
        }

        // Check all the sections for validity and return the binary blob if possible.
        let code_section = code_section.ok_or_else(|| {
            anyhow!(
                "Section '{}' not found, which is required to be present.",
                CODE_SECTION_KEY.0
            )
        })?;

        let data_section = data_section.unwrap_or_else(|| Section {
            start: code_section.start + code_section.length,
            length: 0,
            data: Vec::new(),
            load_address: code_section.load_address + code_section.length,
        });

        let zi_start = data_section.start + data_section.length;
        let zi_address = data_section.load_address + data_section.length;

        let bss_section = bss_section.unwrap_or_else(|| Section {
            start: zi_start,
            length: 0,
            data: Vec::new(),
            load_address: zi_address,
        });
        let static_base = got_section
            .as_ref()
            .map(|got| got.start)
            .unwrap_or(data_section.start);

        let runtime_start = code_section.start;
        let runtime_end = bss_section.start + bss_section.length;

        let mut runtime_sections: Vec<_> = runtime_sections
            .into_iter()
            .filter(|section| {
                section.start >= runtime_start && section.start + section.length <= runtime_end
            })
            .collect();
        runtime_sections.sort_by_key(|section| section.start);

        let mut address_relocation_ranges: Vec<_> = relocation_sections
            .into_iter()
            .filter(|section| {
                section.start >= runtime_start && section.start + section.length <= runtime_end
            })
            .map(|section| RelocationRange {
                offset: section.start - runtime_start,
                size: section.length,
            })
            .collect();
        address_relocation_ranges.sort_by_key(|range| range.offset);

        if runtime_sections.is_empty() {
            return Err(anyhow!(
                "No runtime sections found in the flash algorithm ELF between {runtime_start:#010x} and {runtime_end:#010x}."
            ));
        }

        for pair in runtime_sections.windows(2) {
            let current = &pair[0];
            let next = &pair[1];
            anyhow::ensure!(
                current.start + current.length <= next.start,
                "Flash algorithm sections overlap in memory: {:#010x}..{:#010x} overlaps with {:#010x}..{:#010x}.",
                current.start,
                current.start + current.length,
                next.start,
                next.start + next.length,
            );
        }

        Ok(Self {
            code_section,
            static_base,
            address_relocation_ranges,
            runtime_sections,
            runtime_start,
            runtime_end,
        })
    }

    /// Assembles one contiguous runtime image to write to RAM.
    ///
    /// This preserves any auxiliary loadable sections that sit between `PrgCode` and `PrgData`,
    /// such as `.got`, `.got.plt`, or vendor-specific retained text sections.
    pub(crate) fn blob(&self) -> Vec<u8> {
        let mut blob = vec![0; (self.runtime_end - self.runtime_start) as usize];

        for section in &self.runtime_sections {
            if section.data.is_empty() {
                continue;
            }

            let offset = (section.start - self.runtime_start) as usize;
            let end = offset + section.data.len();
            blob[offset..end].copy_from_slice(&section.data);
        }

        blob
    }

    /// Returns whether the runtime image can be reconstructed by loading a single
    /// contiguous blob at the code section load address.
    pub(crate) fn is_continuous_in_ram(&self) -> bool {
        self.runtime_sections.iter().all(|section| {
            section.load_address >= self.code_section.load_address
                && section.load_address - self.code_section.load_address
                    == section.start - self.runtime_start
        })
    }
}
