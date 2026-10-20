//! Locating the vector table of a loaded image, which decides how it starts.

use object::{
    Architecture, Endianness, Object, ObjectSection, ObjectSymbol, elf::FileHeader32,
    elf::FileHeader64, read::elf::ElfFile, read::elf::FileHeader,
};

use crate::flashing::builder::FlashBuilder;

type Elf32<'data> = ElfFile<'data, FileHeader32<Endianness>>;
type Elf64<'data> = ElfFile<'data, FileHeader64<Endianness>>;

/// Section names that can hold the vector table.
const SECTION_NAMES: &[&str] = &[".vector_table"]; // cortex-m-rt

/// Symbol names that can mark the start of the vector table, for an Arm image
/// whose section carries a name we do not know.
const SYMBOL_NAMES: &[&str] = &[
    "__vector_table", // cortex-m-rt, IAR
    "__Vectors",      // Keil / ARMCLANG
    "__isr_vector",   // NXP MCUXpresso
    "_vector_table",  // Zephyr
];

/// Locate the vector table of an ELF image by name, `None` if the image
/// carries no name we know. `builder` must already hold its staged data.
///
/// The bytes behind the name are not checked. Such a check only fits a
/// Cortex-M table, so it would reject a Cortex-A/R one — and a rejected image
/// is started with a reset, which discards it when it runs from RAM.
pub(super) fn find_in_elf(elf_data: &[u8], builder: &FlashBuilder) -> Option<u64> {
    let addresses = match object::FileKind::parse(elf_data) {
        Ok(object::FileKind::Elf32) => candidates(&Elf32::parse(elf_data).ok()?),
        Ok(object::FileKind::Elf64) => candidates(&Elf64::parse(elf_data).ok()?),
        _ => return None,
    };

    pick(&addresses, builder)
}

/// Candidate addresses for the vector table, most trustworthy first.
fn candidates<T: FileHeader>(binary: &ElfFile<'_, T>) -> Vec<u64> {
    let mut addresses = binary
        .sections()
        .filter(|section| named(section.name(), SECTION_NAMES))
        .map(|section| section.address())
        .collect::<Vec<_>>();

    // These spellings are Arm boot vectors, including AArch64. Other
    // architectures reuse them for an interrupt table — riscv-rt's
    // `_vector_table` — which is not where the image starts.
    if arm_boot_symbols(binary.architecture()) {
        addresses.extend(
            binary
                .symbols()
                .filter(|symbol| !symbol.is_undefined())
                .filter(|symbol| named(symbol.name(), SYMBOL_NAMES))
                .map(|symbol| symbol.address()),
        );
    }

    addresses
}

fn arm_boot_symbols(architecture: Architecture) -> bool {
    matches!(
        architecture,
        Architecture::Arm | Architecture::Aarch64 | Architecture::Aarch64_Ilp32
    )
}

fn named<E>(name: Result<&str, E>, names: &[&str]) -> bool {
    name.is_ok_and(|name| names.contains(&name))
}

/// Return the first candidate address that is part of the image.
fn pick(candidates: &[u64], builder: &FlashBuilder) -> Option<u64> {
    for &address in candidates {
        if builder.has_data_in_range(&(address..address + 1)) {
            tracing::debug!("Vector table at {address:#010x}");
            return Some(address);
        }
        tracing::debug!("Not the vector table, the image stages nothing there: {address:#010x}");
    }

    tracing::debug!("No vector table address could be taken from the image.");
    None
}

#[cfg(test)]
mod test {
    use object::elf::{
        EM_AARCH64, EM_ARM, EM_RISCV, SHN_ABS, SHT_PROGBITS, SHT_STRTAB, SHT_SYMTAB,
    };

    use super::*;

    const TABLE: u64 = 0x2000_0000;
    const OTHER: u64 = 0x0800_0000;
    /// Not a Cortex-M table: the initial stack word is zero.
    const TABLE_BYTES: &[u8] = &[0, 0, 0, 0, 1, 0, 0, 0];

    #[test]
    fn the_first_candidate_the_image_stages_is_used() {
        let mut builder = FlashBuilder::new();
        builder.add_data(TABLE, &[0u8; 8]).unwrap();

        // Nothing is staged at the first address, so the next one is used.
        assert_eq!(pick(&[TABLE - 0x100, TABLE], &builder), Some(TABLE));
        // Both are staged, and `candidates` puts the symbol's address first.
        assert_eq!(pick(&[TABLE, TABLE + 4], &builder), Some(TABLE));
        assert_eq!(pick(&[TABLE - 0x100], &builder), None);
        assert_eq!(pick(&[], &builder), None);
    }

    #[test]
    fn a_riscv_vector_symbol_is_not_a_boot_vector() {
        let builder = staged(&[TABLE]);
        let symbols = [sym("_vector_table", TABLE)];

        assert_eq!(
            find_in_elf(&elf(false, EM_RISCV.0, None, &symbols), &builder),
            None
        );
        assert_eq!(
            find_in_elf(&elf(true, EM_RISCV.0, None, &symbols), &builder),
            None
        );
        // Arm and AArch64 resolve that same symbol.
        assert_eq!(
            find_in_elf(&elf(false, EM_ARM.0, None, &symbols), &builder),
            Some(TABLE)
        );
        assert_eq!(
            find_in_elf(&elf(true, EM_AARCH64.0, None, &symbols), &builder),
            Some(TABLE)
        );
    }

    #[test]
    fn arm_images_keep_every_symbol_spelling() {
        let builder = staged(&[TABLE]);
        for name in [
            "__vector_table",
            "__Vectors",
            "__isr_vector",
            "_vector_table",
        ] {
            let symbols = [sym(name, TABLE)];
            assert_eq!(
                find_in_elf(&elf(false, EM_ARM.0, None, &symbols), &builder),
                Some(TABLE),
                "{name}"
            );
            assert_eq!(
                find_in_elf(&elf(true, EM_AARCH64.0, None, &symbols), &builder),
                Some(TABLE),
                "{name}"
            );
        }
    }

    #[test]
    fn a_vector_table_section_keeps_priority() {
        let builder = staged(&[TABLE, OTHER]);
        let symbols = [sym("_vector_table", OTHER)];
        for (class64, machine) in [
            (false, EM_ARM.0),
            (true, EM_AARCH64.0),
            (false, EM_RISCV.0),
            (true, EM_RISCV.0),
        ] {
            assert_eq!(
                find_in_elf(&elf(class64, machine, Some(TABLE), &symbols), &builder),
                Some(TABLE),
                "machine {machine:#x} class64 {class64}"
            );
        }

        // Nothing is staged for the section, so an Arm image takes the later symbol.
        let builder = staged(&[OTHER]);
        let symbols = [sym("__vector_table", OTHER)];
        assert_eq!(
            find_in_elf(&elf(false, EM_ARM.0, Some(TABLE), &symbols), &builder),
            Some(OTHER)
        );
        assert_eq!(
            find_in_elf(&elf(true, EM_AARCH64.0, Some(TABLE), &symbols), &builder),
            Some(OTHER)
        );
        // RISC-V has no symbol fallback once the section address is unstaged.
        assert_eq!(
            find_in_elf(&elf(false, EM_RISCV.0, Some(TABLE), &symbols), &builder),
            None
        );
        assert_eq!(
            find_in_elf(&elf(true, EM_RISCV.0, Some(TABLE), &symbols), &builder),
            None
        );

        // Undefined symbols are ignored. An address with no staged data is ignored
        // too, and a later staged symbol is used.
        let builder = staged(&[TABLE]);
        assert_eq!(
            find_in_elf(
                &elf(false, EM_ARM.0, None, &[("_vector_table", TABLE, false)]),
                &builder
            ),
            None
        );
        assert_eq!(
            find_in_elf(
                &elf(
                    false,
                    EM_ARM.0,
                    None,
                    &[("__Vectors", OTHER, true), sym("__isr_vector", TABLE)]
                ),
                &builder
            ),
            Some(TABLE)
        );
        assert_eq!(
            find_in_elf(
                &elf(true, EM_AARCH64.0, None, &[sym("__vector_table", OTHER)]),
                &builder
            ),
            None
        );
    }

    #[test]
    fn malformed_images_have_no_vector_table() {
        let builder = staged(&[TABLE]);
        assert_eq!(find_in_elf(&[], &builder), None);
        assert_eq!(find_in_elf(b"this is not elf!", &builder), None);

        let mut short = [0u8; 16];
        short[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        short[4] = 1;
        short[5] = 1;
        short[6] = 1;
        assert_eq!(find_in_elf(&short, &builder), None);

        let mut broken = vec![0u8; 52];
        broken[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        broken[4] = 1;
        broken[5] = 1;
        broken[6] = 1;
        broken[32..36].copy_from_slice(&1000u32.to_le_bytes());
        broken[46..48].copy_from_slice(&40u16.to_le_bytes());
        broken[48..50].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(find_in_elf(&broken, &builder), None);
    }

    fn staged(addresses: &[u64]) -> FlashBuilder {
        let mut builder = FlashBuilder::new();
        for &address in addresses {
            builder.add_data(address, TABLE_BYTES).unwrap();
        }
        builder
    }

    fn sym(name: &'static str, address: u64) -> (&'static str, u64, bool) {
        (name, address, true)
    }

    /// Little-endian ELF with an optional `.vector_table` section and symbols.
    ///
    /// A defined symbol is absolute, so its value is the address `pick` sees.
    fn elf(
        class64: bool,
        machine: u16,
        section: Option<u64>,
        symbols: &[(&str, u64, bool)],
    ) -> Vec<u8> {
        let mut section_names = vec![0];
        let section_name = cstr(&mut section_names, ".vector_table");
        let symtab_name = cstr(&mut section_names, ".symtab");
        let strtab_name = cstr(&mut section_names, ".strtab");
        let shstrtab_name = cstr(&mut section_names, ".shstrtab");

        let mut symbol_names = vec![0];
        let symbol_name_offsets: Vec<u32> = symbols
            .iter()
            .map(|(name, _, _)| cstr(&mut symbol_names, name))
            .collect();

        let ehsize = if class64 { 64 } else { 52 };
        let mut body = Vec::new();
        let table_off = ehsize + body.len();
        if section.is_some() {
            body.extend_from_slice(TABLE_BYTES);
        }
        let names_off = ehsize + body.len();
        body.extend_from_slice(&section_names);
        let strings_off = ehsize + body.len();
        body.extend_from_slice(&symbol_names);

        let sym_size = if class64 { 24 } else { 16 };
        let sym_off = ehsize + body.len();
        body.extend_from_slice(&vec![0; sym_size]);
        for ((_, address, defined), name) in symbols.iter().zip(symbol_name_offsets) {
            body.extend(symbol(class64, name, *address, *defined));
        }

        let shoff = ehsize + body.len();
        body.extend(shdr(class64, 0, 0, 0, 0, 0, 0, 0, 0));
        body.extend(shdr(
            class64,
            shstrtab_name,
            SHT_STRTAB.0,
            0,
            names_off as u64,
            section_names.len() as u64,
            0,
            0,
            0,
        ));
        body.extend(shdr(
            class64,
            strtab_name,
            SHT_STRTAB.0,
            0,
            strings_off as u64,
            symbol_names.len() as u64,
            0,
            0,
            0,
        ));
        body.extend(shdr(
            class64,
            symtab_name,
            SHT_SYMTAB.0,
            0,
            sym_off as u64,
            (sym_size * (symbols.len() + 1)) as u64,
            2,
            1,
            sym_size as u64,
        ));
        if let Some(address) = section {
            body.extend(shdr(
                class64,
                section_name,
                SHT_PROGBITS.0,
                address,
                table_off as u64,
                TABLE_BYTES.len() as u64,
                0,
                0,
                0,
            ));
        }

        let shnum = if section.is_some() { 5 } else { 4 };
        let mut file = ehdr(class64, machine, shoff as u64, shnum, 1);
        file.extend(body);
        file
    }

    fn cstr(buf: &mut Vec<u8>, text: &str) -> u32 {
        let offset = u32::try_from(buf.len()).unwrap();
        buf.extend_from_slice(text.as_bytes());
        buf.push(0);
        offset
    }

    fn symbol(class64: bool, name: u32, address: u64, defined: bool) -> Vec<u8> {
        let shndx = if defined { SHN_ABS.0 } else { 0 };
        let mut out = Vec::new();
        push_u32(&mut out, name);
        if class64 {
            out.push(0x11);
            out.push(0);
            push_u16(&mut out, shndx);
            push_u64(&mut out, address);
            push_u64(&mut out, 4);
        } else {
            push_u32(&mut out, u32::try_from(address).unwrap());
            push_u32(&mut out, 4);
            out.push(0x11);
            out.push(0);
            push_u16(&mut out, shndx);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn shdr(
        class64: bool,
        name: u32,
        kind: u32,
        addr: u64,
        offset: u64,
        size: u64,
        link: u32,
        info: u32,
        entsize: u64,
    ) -> Vec<u8> {
        let mut out = Vec::new();
        push_u32(&mut out, name);
        push_u32(&mut out, kind);
        if class64 {
            push_u64(&mut out, 0);
            push_u64(&mut out, addr);
            push_u64(&mut out, offset);
            push_u64(&mut out, size);
            push_u32(&mut out, link);
            push_u32(&mut out, info);
            push_u64(&mut out, 0);
            push_u64(&mut out, entsize);
        } else {
            push_u32(&mut out, 0);
            push_u32(&mut out, u32::try_from(addr).unwrap());
            push_u32(&mut out, u32::try_from(offset).unwrap());
            push_u32(&mut out, u32::try_from(size).unwrap());
            push_u32(&mut out, link);
            push_u32(&mut out, info);
            push_u32(&mut out, 0);
            push_u32(&mut out, u32::try_from(entsize).unwrap());
        }
        out
    }

    fn ehdr(class64: bool, machine: u16, shoff: u64, shnum: u16, shstrndx: u16) -> Vec<u8> {
        let mut out = vec![0; if class64 { 64 } else { 52 }];
        out[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
        out[4] = if class64 { 2 } else { 1 };
        out[5] = 1;
        out[6] = 1;
        put_u16(&mut out, 16, 2);
        put_u16(&mut out, 18, machine);
        put_u32(&mut out, 20, 1);
        if class64 {
            put_u64(&mut out, 40, shoff);
            put_u16(&mut out, 52, 64);
            put_u16(&mut out, 58, 64);
            put_u16(&mut out, 60, shnum);
            put_u16(&mut out, 62, shstrndx);
        } else {
            put_u32(&mut out, 32, u32::try_from(shoff).unwrap());
            put_u16(&mut out, 40, 52);
            put_u16(&mut out, 46, 40);
            put_u16(&mut out, 48, shnum);
            put_u16(&mut out, 50, shstrndx);
        }
        out
    }

    fn push_u16(buf: &mut Vec<u8>, value: u16) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn push_u64(buf: &mut Vec<u8>, value: u64) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u16(buf: &mut [u8], at: usize, value: u16) {
        buf[at..at + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u32(buf: &mut [u8], at: usize, value: u32) {
        buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put_u64(buf: &mut [u8], at: usize, value: u64) {
        buf[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
}
