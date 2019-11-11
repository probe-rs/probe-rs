use goblin::elf::program_header::PT_LOAD;   
use num_traits::FromPrimitive;
use num_derive::FromPrimitive;
use probe_rs::probe::flash::memory::MemoryRange;

const RO: (&str, Option<SectionType>) = ("PrgCode", Some(SectionType::SHT_PROGBITS));
const RW: (&str, Option<SectionType>) = ("PrgData", Some(SectionType::SHT_PROGBITS));
const ZI: (&str, Option<SectionType>) = ("PrgData", Some(SectionType::SHT_NOBITS));

#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Eq, Clone, Copy, FromPrimitive)]
enum SectionType {
    SHT_PROGBITS = 1,
    SHT_NOBITS = 8,
    DEFAULT,
}

#[derive(Debug, Clone)]
pub struct Section {
    pub start: u32,
    pub length: u32,
    pub data: Vec<u8>,
}

impl Section {
    pub fn empty(start: u32) -> Self {
        Self {
            start,
            length: 0,
            data: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AlgorithmBinary {
    pub ro: Section,
    pub rw: Section,
    pub zi: Section,
    pub blob: Vec<u8>,
}

impl AlgorithmBinary {
    pub fn new(elf: &goblin::elf::Elf<'_>, buffer: &[u8]) -> Self {
        let mut ro = None;
        let mut rw = None;
        let mut zi = None;

        for ph in &elf.program_headers {
            if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                let sector = ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

                for sh in &elf.section_headers {
                    let range = sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32;
                    if sector.contains_range(&range) {
                        dbg!(sh);
                        let mut data = Vec::from(&buffer[sh.sh_offset as usize..][..sh.sh_size as usize]);
                        let section = Some(Section {
                            start: sh.sh_addr as u32,
                            length: sh.sh_size as u32,
                            data,
                        });
                        match (&elf.shdr_strtab[sh.sh_name], FromPrimitive::from_u32(sh.sh_type)) {
                            RO => ro = section,
                            RW => rw = section,
                            ZI => zi = section,
                            _ => {},
                        }
                    }
                }
            }
        }

        let mut blob = Vec::new();

        let ro = ro.unwrap();
        blob.extend(&ro.data);

        let rw = rw.unwrap();
        blob.extend(&rw.data);

        let zi = zi.unwrap();
        blob.extend(&vec![0; zi.length as usize]);

        println!("{}/{}, {}/{}, {}/{}", ro.start, ro.length, rw.start, rw.length, zi.start, zi.length);

        Self {
            ro,
            rw,
            zi,
            blob,
        }
    }
}