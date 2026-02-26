// This code is taken from samply-object and samply-debugid in the [samply
// codebase](https://github.com/mstange/samply)
// Dual licensed under Apache-2.0 and MIT.
// Code not relevant to ELF files has been removed.
// TODO: replace this module with samply-object library form crates.io when released.

use std::str::FromStr;

use debugid::DebugId;
use object::{FileFlags, Object, ObjectSection};
use uuid::Uuid;

/// Tries to obtain a CodeId for an object.
///
/// This currently only handles mach-O and ELF.
pub(crate) fn code_id_for_object<'data>(obj: &impl Object<'data>) -> Option<CodeId> {
    // ELF
    if let Ok(Some(build_id)) = obj.build_id() {
        return Some(CodeId::ElfBuildId(ElfBuildId::from_bytes(build_id)));
    }

    None
}

/// Tries to obtain a DebugId for an object. This uses the build ID, if available,
/// and falls back to hashing the first page of the text section otherwise.
/// Returns None on failure.
pub(crate) fn debug_id_for_object<'data>(obj: &impl Object<'data>) -> Option<DebugId> {
    // ELF
    if let Ok(Some(build_id)) = obj.build_id() {
        return Some(DebugId::from_identifier(build_id, obj.is_little_endian()));
    }
    // We were not able to locate a build ID, so fall back to creating a synthetic
    // identifier from a hash of the first page of the ".text" (program code) section.
    if let Some(section) = obj.section_by_name(".text") {
        let data_len = section.size().min(4096);
        if let Ok(Some(first_page_data)) = section.data_range(section.address(), data_len) {
            return Some(DebugId::from_text_first_page(
                first_page_data,
                obj.is_little_endian(),
            ));
        }
    }

    None
}

/// The "relative address base" is the base address which [`LookupAddress::Relative`](https://docs.rs/samply-symbols/latest/samply_symbols/enum.LookupAddress.html#variant.Relative)
/// addresses are relative to. You start with an SVMA (a stated virtual memory address),
/// you subtract the relative address base, and out comes a relative address.
///
/// This function computes that base address. It is defined as follows:
///
///  - For ELF binaries, the base address is the vmaddr of the *first* segment,
///    i.e. the vmaddr of the first "LOAD" ELF command.
///
/// In many cases, this base address is simply zero:
///
///  - ELF images of dynamic libraries (i.e. not executables) usually have a
///    base address of zero.
///
/// However, in the following cases, the base address is usually non-zero:
///
///  - ELF executables can have non-zero base addresses, e.g. 0x200000 or 0x400000.
///  - Kernel ELF binaries ("vmlinux") have a large base address such as
///    0xffffffff81000000. Moreover, the base address seems to coincide with the
///    vmaddr of the .text section, which is readily-available in perf.data files
///    (in a synthetic mapping called "[kernel.kallsyms]_text").
pub(crate) fn relative_address_base<'data>(obj: &impl Object<'data>) -> u64 {
    use object::read::ObjectSegment;
    if let FileFlags::Elf { .. } = obj.flags() {
        // This is an ELF image. "Relative addresses" are relative to the
        // vmaddr of the first segment (the first LOAD command).
        if let Some(first_segment) = obj.segments().next() {
            return first_segment.address();
        }
    }

    // For PE binaries, relative_address_base() returns the image base address.
    obj.relative_address_base()
}

trait DebugIdExt {
    /// Creates a DebugId from some identifier. The identifier could be
    /// an ELF build ID, or a hash derived from the text section.
    /// The `little_endian` argument specifies whether the object file
    /// is targeting a little endian architecture.
    fn from_identifier(identifier: &[u8], little_endian: bool) -> Self;

    /// Creates a DebugId from a hash of the first 4096 bytes of the .text section.
    /// The `little_endian` argument specifies whether the object file
    /// is targeting a little endian architecture.
    fn from_text_first_page(text_first_page: &[u8], little_endian: bool) -> Self;
}

impl DebugIdExt for DebugId {
    fn from_identifier(identifier: &[u8], little_endian: bool) -> Self {
        // Make sure that we have exactly 16 bytes available, either truncate or fill
        // the remainder with zeros.
        // ELF build IDs are usually 20 bytes, so if the identifier is an ELF build ID
        // then we're performing a lossy truncation.
        let mut d = [0u8; 16];
        let shared_len = identifier.len().min(d.len());
        d[0..shared_len].copy_from_slice(&identifier[0..shared_len]);

        // Pretend that the build ID was stored as a UUID with u32 u16 u16 fields inside
        // the file. Parse those fields in the endianness of the file. Then use
        // Uuid::from_fields to serialize them as big endian.
        // For ELF build IDs this is a bit silly, because ELF build IDs aren't actually
        // field-based UUIDs, but this is what the tools in the breakpad and
        // sentry/symbolic universe do, so we do the same for compatibility with those
        // tools.
        let (d1, d2, d3) = if little_endian {
            (
                u32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                u16::from_le_bytes([d[4], d[5]]),
                u16::from_le_bytes([d[6], d[7]]),
            )
        } else {
            (
                u32::from_be_bytes([d[0], d[1], d[2], d[3]]),
                u16::from_be_bytes([d[4], d[5]]),
                u16::from_be_bytes([d[6], d[7]]),
            )
        };
        let uuid = Uuid::from_fields(d1, d2, d3, d[8..16].try_into().unwrap());
        DebugId::from_uuid(uuid)
    }

    // This algorithm XORs 16-byte chunks directly into a 16-byte buffer.
    fn from_text_first_page(text_first_page: &[u8], little_endian: bool) -> Self {
        const UUID_SIZE: usize = 16;
        const PAGE_SIZE: usize = 4096;
        let mut hash = [0; UUID_SIZE];
        for (i, byte) in text_first_page.iter().cloned().take(PAGE_SIZE).enumerate() {
            hash[i % UUID_SIZE] ^= byte;
        }
        DebugId::from_identifier(&hash, little_endian)
    }
}

/// An enum carrying an identifier for a binary. This is stores the same information
/// as a [`debugid::CodeId`], but without projecting it down to a string.
///
/// All types need to be treated rather differently, see their respective documentation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum CodeId {
    /// The code ID for a Linux ELF file. This is the "ELF build ID" (also called "GNU build ID").
    /// The build ID is usually 20 bytes, commonly written out as 40 hex chars.
    ///
    /// It can be used to find debug files on the local file system or to download
    /// binaries or debug files from a `debuginfod` symbol server. it does not have to be
    /// paired with the binary name.
    ///
    /// An ELF binary's code ID is more useful than its debug ID: The debug ID is truncated
    /// to 16 bytes (32 hex characters), whereas the code ID is the full ELF build ID.
    ElfBuildId(ElfBuildId),
}

impl FromStr for CodeId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // ELF build ID. These are usually 40 hex characters (= 20 bytes).
        Ok(CodeId::ElfBuildId(ElfBuildId::from_str(s)?))
    }
}

impl std::fmt::Display for CodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CodeId::ElfBuildId(elf) => std::fmt::Display::fmt(elf, f),
        }
    }
}

/// The build ID for an ELF file (also called "GNU build ID").
///
/// The build ID can be used to find debug files on the local file system or to download
/// binaries or debug files from a `debuginfod` symbol server. it does not have to be
/// paired with the binary name.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct ElfBuildId(Vec<u8>);

impl ElfBuildId {
    /// Create a new `ElfBuildId` from a slice of bytes (commonly a sha1 hash
    /// generated by the linker, i.e. 20 bytes).
    fn from_bytes(bytes: &[u8]) -> Self {
        Self(bytes.to_owned())
    }
}

impl FromStr for ElfBuildId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let byte_count = s.len() / 2;
        let mut bytes = Vec::with_capacity(byte_count);
        for i in 0..byte_count {
            let hex_byte = &s[i * 2..i * 2 + 2];
            let b = u8::from_str_radix(hex_byte, 16).map_err(|_| ())?;
            bytes.push(b);
        }
        Ok(Self(bytes))
    }
}

impl std::fmt::Display for ElfBuildId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in &self.0 {
            f.write_fmt(format_args!("{byte:02x}"))?;
        }
        Ok(())
    }
}
