//! Locating the vector table of a loaded image, which decides how it starts.

use object::{
    Endianness, Object, ObjectSection, ObjectSymbol, elf::FileHeader32, elf::FileHeader64,
    read::elf::ElfFile, read::elf::FileHeader,
};

use crate::flashing::builder::FlashBuilder;

type Elf32<'data> = ElfFile<'data, FileHeader32<Endianness>>;
type Elf64<'data> = ElfFile<'data, FileHeader64<Endianness>>;

/// Section names that can hold the vector table.
const SECTION_NAMES: &[&str] = &[".vector_table"]; // cortex-m-rt

/// Symbol names that can mark the start of the vector table, for an image
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
    let sections = binary
        .sections()
        .filter(|section| named(section.name(), SECTION_NAMES))
        .map(|section| section.address());
    let symbols = binary
        .symbols()
        .filter(|symbol| !symbol.is_undefined())
        .filter(|symbol| named(symbol.name(), SYMBOL_NAMES))
        .map(|symbol| symbol.address());

    sections.chain(symbols).collect()
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
    use super::*;

    #[test]
    fn the_first_candidate_the_image_stages_is_used() {
        const TABLE: u64 = 0x2000_0000;
        let mut builder = FlashBuilder::new();
        builder.add_data(TABLE, &[0u8; 8]).unwrap();

        // Nothing is staged at the first address, so the next one is used.
        assert_eq!(pick(&[TABLE - 0x100, TABLE], &builder), Some(TABLE));
        // Both are staged, and `candidates` puts the symbol's address first.
        assert_eq!(pick(&[TABLE, TABLE + 4], &builder), Some(TABLE));
        assert_eq!(pick(&[TABLE - 0x100], &builder), None);
        assert_eq!(pick(&[], &builder), None);
    }
}
