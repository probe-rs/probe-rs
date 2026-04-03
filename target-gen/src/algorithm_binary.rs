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
    pub(crate) data_section: Section,
    pub(crate) static_base: u32,
    pub(crate) link_time_base_address: u32,
    pub(crate) address_relocations: Vec<Relocation>,
    pub(crate) runtime_sections: Vec<Section>,
    pub(crate) runtime_start: u32,
    pub(crate) runtime_end: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Relocation {
    pub(crate) offset: u32,
}

impl AlgorithmBinary {
    /// Extract a new flash algorithm binary blob from an ELF data blob.
    pub(crate) fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8]) -> Result<Self> {
        let mut code_section = None;
        let mut data_section = None;
        let mut bss_section = None;
        let mut got_section = None;
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
                            }
                            GOT_PLT_SECTION_KEY => got_section = got_section.or(section),
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

        let address_relocations = collect_address_relocations(elf, runtime_start, runtime_end)?;

        Ok(Self {
            code_section,
            data_section,
            static_base,
            link_time_base_address: runtime_start,
            address_relocations,
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

fn collect_address_relocations(
    elf: &goblin::elf::Elf<'_>,
    runtime_start: u32,
    runtime_end: u32,
) -> Result<Vec<Relocation>> {
    let mut relocations = Vec::new();

    for (_section_index, section_relocs) in &elf.shdr_relocs {
        for reloc in section_relocs.iter() {
            let Some(offset) = u32::try_from(reloc.r_offset).ok() else {
                continue;
            };

            match relocation_offset(runtime_start, runtime_end, offset)? {
                Some(offset) => relocations.push(Relocation { offset }),
                None => continue,
            }
        }
    }

    relocations.sort_by_key(|reloc| reloc.offset);
    relocations.dedup_by_key(|reloc| reloc.offset);

    Ok(relocations)
}

fn relocation_offset(runtime_start: u32, runtime_end: u32, offset: u32) -> Result<Option<u32>> {
    let in_bounds = offset >= runtime_start
        && offset
            .checked_add(4)
            .is_some_and(|offset_end| offset_end <= runtime_end);

    if !in_bounds {
        return Ok(None);
    }

    anyhow::ensure!(
        (offset % 4) == 0,
        "Flash algorithm relocation slot {offset:#010x} is not 4-byte aligned."
    );

    Ok(Some(offset - runtime_start))
}

#[cfg(test)]
mod test {
    use goblin::elf::reloc::Reloc;

    use super::{Relocation, relocation_offset};

    #[test]
    fn relocation_offset_converts_absolute_slot_to_runtime_relative_offset() {
        assert_eq!(
            relocation_offset(0x1000, 0x1100, 0x1008).unwrap(),
            Some(0x8)
        );
    }

    #[test]
    fn relocation_offset_skips_slots_outside_runtime_image() {
        assert_eq!(relocation_offset(0x1000, 0x1100, 0x0ffc).unwrap(), None);
        assert_eq!(relocation_offset(0x1000, 0x1100, 0x1100).unwrap(), None);
    }

    #[test]
    fn relocation_offset_rejects_unaligned_slots() {
        let err = relocation_offset(0x1000, 0x1100, 0x1002).unwrap_err();
        assert!(err.to_string().contains("not 4-byte aligned"));
    }

    #[test]
    fn relocation_records_are_sortable_and_deduplicated() {
        let mut relocations = [
            Relocation { offset: 0x10 },
            Relocation { offset: 0x4 },
            Relocation { offset: 0x10 },
        ]
        .to_vec();
        relocations.sort_by_key(|reloc| reloc.offset);
        relocations.dedup_by_key(|reloc| reloc.offset);

        assert_eq!(
            relocations,
            vec![Relocation { offset: 0x4 }, Relocation { offset: 0x10 }]
        );
    }

    #[test]
    fn goblin_reloc_offsets_match_helper_expectations() {
        let reloc = Reloc {
            r_offset: 0x100c,
            r_addend: None,
            r_sym: 0,
            r_type: 0,
        };

        assert_eq!(
            relocation_offset(0x1000, 0x1100, u32::try_from(reloc.r_offset).unwrap()).unwrap(),
            Some(0xc)
        );
    }
}
