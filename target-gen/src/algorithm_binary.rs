use goblin::{
    elf::program_header::PT_LOAD,
    elf64::section_header::{SHT_NOBITS, SHT_PROGBITS},
};
use probe_rs::config::MemoryRange;

use anyhow::{anyhow, Result};

const CODE_SECTION_KEY: (&str, u32) = ("PrgCode", SHT_PROGBITS);
const DATA_SECTION_KEY: (&str, u32) = ("PrgData", SHT_PROGBITS);
const BSS_SECTION_KEY: (&str, u32) = ("PrgData", SHT_NOBITS);

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
}

/// A struct to hold all the binary sections of a flash algorithm ELF that go into flash.
#[derive(Debug, Clone)]
pub(crate) struct AlgorithmBinary {
    pub(crate) code_section: Section,
    pub(crate) data_section: Section,
    pub(crate) bss_section: Section,
}

impl AlgorithmBinary {
    /// Extract a new flash algorithm binary blob from an ELF data blob.
    pub(crate) fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8]) -> Result<Self> {
        let mut code_section = None;
        let mut data_section = None;
        let mut bss_section = None;

        let mut suspicious_sections = Vec::new();

        // Iterate all program headers and get sections.
        for ph in &elf.program_headers {
            // Only regard sections that contain at least one byte.
            // And are marked loadable (this filters out debug symbols).
            if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                let sector = ph.p_offset..ph.p_offset + ph.p_filesz;

                // Scan all sectors if they contain any part of the sections found.
                for sh in &elf.section_headers {
                    let range = sh.sh_offset..sh.sh_offset + sh.sh_size;
                    if sector.contains_range(&range) {
                        // If we found a valid section, store its contents.
                        let data =
                            Vec::from(&buffer[sh.sh_offset as usize..][..sh.sh_size as usize]);
                        let section = Some(Section {
                            start: sh.sh_addr as u32,
                            length: sh.sh_size as u32,
                            data,
                        });

                        // Make sure we store the section contents under the right name.
                        match (&elf.shdr_strtab[sh.sh_name], sh.sh_type) {
                            CODE_SECTION_KEY => code_section = section,
                            DATA_SECTION_KEY => data_section = section,
                            BSS_SECTION_KEY => bss_section = section,
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
            log::warn!("The ELF file contains some unexpected sections, which should not be part of a flash loader: ");

            for section in suspicious_sections {
                log::warn!("\t{}", section);
            }

            log::warn!("Code should be placed in the '{}' section, and data should be placed in the '{}' section.", CODE_SECTION_KEY.0, DATA_SECTION_KEY.0);
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
        });

        let zi_start = data_section.start + data_section.length;

        Ok(Self {
            code_section,
            data_section,
            bss_section: bss_section.unwrap_or_else(|| Section {
                start: zi_start,
                length: 0,
                data: Vec::new(),
            }),
        })
    }

    /// Assembles one huge binary blob as u8 values to write to RAM from the three sections.
    pub(crate) fn blob(&self) -> Vec<u8> {
        let mut blob = Vec::new();

        blob.extend(&self.code_section.data);
        blob.extend(&self.data_section.data);
        blob.extend(&vec![0; self.bss_section.length as usize]);

        blob
    }
}
