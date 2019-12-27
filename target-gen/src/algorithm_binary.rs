use crate::error::Error;
use goblin::elf::program_header::PT_LOAD;
use num_derive::FromPrimitive;
use num_traits::FromPrimitive;
use probe_rs::config::memory::MemoryRange;

const CODE_SECTION_KEY: (&str, Option<SectionType>) = ("PrgCode", Some(SectionType::SHT_PROGBITS));
const DATA_SECTION_KEY: (&str, Option<SectionType>) = ("PrgData", Some(SectionType::SHT_PROGBITS));
const BSS_SECTION_KEY: (&str, Option<SectionType>) = ("PrgData", Some(SectionType::SHT_NOBITS));

/// An enum to parse the section type from the ELF.
#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive)]
enum SectionType {
    SHT_PROGBITS = 1,
    SHT_NOBITS = 8,
    DEFAULT,
}

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
    pub(crate) fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8]) -> Result<Self, Error> {
        let mut code_section = None;
        let mut data_section = None;
        let mut bss_section = None;

        // Iterate all program headers and get sections.
        for ph in &elf.program_headers {
            // Only regard sections that contain at least one byte.
            // And are marked loadable (this filters out debug symbols).
            if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                let sector = ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

                // Scan all sectors if they contain any part of the sections found.
                for sh in &elf.section_headers {
                    let range = sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32;
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
                        match (
                            &elf.shdr_strtab[sh.sh_name],
                            FromPrimitive::from_u32(sh.sh_type),
                        ) {
                            CODE_SECTION_KEY => code_section = section,
                            DATA_SECTION_KEY => data_section = section,
                            BSS_SECTION_KEY => bss_section = section,
                            _ => {}
                        }
                    }
                }
            }
        }

        // Check all the sections for validity and return the binary blob if possible.
        let code_section = code_section.ok_or_else(|| Error::SectionNotFound("code"))?;
        let data_section = data_section.ok_or_else(|| Error::SectionNotFound("data"))?;
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

    /// Assembles one huge binary blob as u32 values to write to RAM from the three sections.
    pub(crate) fn blob_as_u32(&self) -> Vec<u32> {
        use scroll::Pread;

        self.blob()
            .chunks(4)
            .map(|bytes| bytes.pread(0).unwrap())
            .collect()
    }
}
