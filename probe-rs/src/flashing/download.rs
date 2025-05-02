use object::{
    Endianness, Object, ObjectSection, elf::FileHeader32, elf::FileHeader64, elf::PT_LOAD,
    read::elf::ElfFile, read::elf::FileHeader, read::elf::ProgramHeader,
};
use probe_rs_target::{InstructionSet, MemoryRange};
use serde::{Deserialize, Serialize};

use std::{
    fs::File,
    path::{Path, PathBuf},
    str::FromStr,
};

use super::*;
use crate::session::Session;

/// Extended options for flashing a binary file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    pub base_address: Option<u64>,
    /// The number of bytes to skip at the start of the binary file.
    pub skip: u32,
}

/// Extended options for flashing a ESP-IDF format file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct IdfOptions {
    /// The bootloader
    pub bootloader: Option<PathBuf>,
    /// The partition table
    pub partition_table: Option<PathBuf>,
    /// The target app partition
    pub target_app_partition: Option<String>,
}

/// Extended options for flashing an ELF file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Default)]
pub struct ElfOptions {
    /// Sections to skip flashing
    pub skip_sections: Vec<String>,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq, Clone, Copy)]
pub enum FormatKind {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin,
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    #[default]
    Elf,
    /// Marks a file in the [ESP-IDF bootloader](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/app_image_format.html#app-image-structures) format.
    /// Use [IdfOptions] to configure flashing.
    Idf,
    /// Marks a file in the [UF2](https://github.com/microsoft/uf2) format.
    Uf2,
}

impl FormatKind {
    /// Creates a new Format from an optional string.
    ///
    /// If the string is `None`, the default format is returned.
    pub fn from_optional(s: Option<&str>) -> Result<Self, String> {
        match s {
            Some(format) => Self::from_str(format),
            None => Ok(Self::default()),
        }
    }
}

impl FromStr for FormatKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_lowercase()[..] {
            "bin" | "binary" => Ok(Self::Bin),
            "hex" | "ihex" | "intelhex" => Ok(Self::Hex),
            "elf" => Ok(Self::Elf),
            "uf2" => Ok(Self::Uf2),
            "idf" | "esp-idf" | "espidf" => Ok(Self::Idf),
            _ => Err(format!("Format '{s}' is unknown.")),
        }
    }
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone)]
pub enum Format {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin(BinOptions),
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    Elf(ElfOptions),
    /// Marks a file in the [ESP-IDF bootloader](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/system/app_image_format.html#app-image-structures) format.
    /// Use [IdfOptions] to configure flashing.
    Idf(IdfOptions),
    /// Marks a file in the [UF2](https://github.com/microsoft/uf2) format.
    Uf2,
}

impl Default for Format {
    fn default() -> Self {
        Format::Elf(ElfOptions::default())
    }
}

impl From<FormatKind> for Format {
    fn from(kind: FormatKind) -> Self {
        match kind {
            FormatKind::Bin => Format::Bin(BinOptions::default()),
            FormatKind::Hex => Format::Hex,
            FormatKind::Elf => Format::Elf(ElfOptions::default()),
            FormatKind::Uf2 => Format::Uf2,
            FormatKind::Idf => Format::Idf(IdfOptions::default()),
        }
    }
}

/// A finite list of all the errors that can occur when flashing a given file.
///
/// This includes corrupt file issues,
/// OS permission issues as well as chip connectivity and memory boundary issues.
#[derive(Debug, thiserror::Error, docsplay::Display)]
pub enum FileDownloadError {
    /// An error with the flashing procedure has occurred.
    #[ignore_extra_doc_attributes]
    ///
    /// This is mostly an error in the communication with the target inflicted by a bad hardware connection or a probe-rs bug.
    Flash(#[from] FlashError),

    /// Failed to read or decode the IHEX file.
    IhexRead(#[from] ihex::ReaderError),

    /// An IO error has occurred while reading the firmware file.
    IO(#[from] std::io::Error),

    /// Error while reading the object file: {0}.
    Object(&'static str),

    /// Failed to read or decode the ELF file.
    Elf(#[from] object::read::Error),

    /// Failed to format as esp-idf binary
    Idf(#[from] espflash::error::Error),

    /// Target {0} does not support the esp-idf format
    IdfUnsupported(String),

    /// No loadable segments were found in the ELF file.
    #[ignore_extra_doc_attributes]
    ///
    /// This is most likely because of a bad linker script.
    NoLoadableSegments,

    /// Could not determine flash size.
    FlashSizeDetection(#[from] crate::Error),

    /// The image ({image:?}) is not compatible with the target ({print_instr_sets(target)}).
    IncompatibleImage {
        /// The target's instruction set.
        target: Vec<InstructionSet>,
        /// The image's instruction set.
        image: InstructionSet,
    },

    /// An error occurred during download.
    Other(#[source] crate::Error),
}

fn print_instr_sets(instr_sets: &[InstructionSet]) -> String {
    instr_sets
        .iter()
        .map(|instr_set| format!("{instr_set:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Options for downloading a file onto a target chip.
///
/// This struct should be created using the [`DownloadOptions::default()`] function, and can be configured by setting
/// the fields directly:
///
/// ```
/// use probe_rs::flashing::DownloadOptions;
///
/// let mut options = DownloadOptions::default();
///
/// options.verify = true;
/// ```
#[derive(Default)]
#[non_exhaustive]
pub struct DownloadOptions<'p> {
    /// An optional progress reporter which is used if this argument is set to `Some(...)`.
    pub progress: Option<FlashProgress<'p>>,
    /// If `keep_unwritten_bytes` is `true`, erased portions of the flash that are not overwritten by the ELF data
    /// are restored afterwards, such that the old contents are untouched.
    ///
    /// This is necessary because the flash can only be erased in sectors. If only parts of the erased sector are written thereafter,
    /// instead of the full sector, the excessively erased bytes wont match the contents before the erase which might not be intuitive
    /// to the user or even worse, result in unexpected behavior if those contents contain important data.
    pub keep_unwritten_bytes: bool,
    /// Perform a dry run. This prepares everything for flashing, but does not write anything to flash.
    pub dry_run: bool,
    /// If this flag is set to true, probe-rs will try to use the chips built in method to do a full chip erase if one is available.
    /// This is often faster than erasing a lot of single sectors.
    /// So if you do not need the old contents of the flash, this is a good option.
    pub do_chip_erase: bool,
    /// If the chip was pre-erased with external erasers, this flag can set to true to skip erasing
    /// It may be useful for mass production.
    pub skip_erase: bool,
    /// Before flashing, read back the flash contents to skip up-to-date regions.
    pub preverify: bool,
    /// After flashing, read back all the flashed data to verify it has been written correctly.
    pub verify: bool,
    /// Disable double buffering when loading flash.
    pub disable_double_buffering: bool,
}

impl DownloadOptions<'_> {
    /// DownloadOptions with default values.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Builds a new flash loader for the given target and path. This
/// will check the path for validity and check what pages have to be
/// flashed etc.
pub fn build_loader(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: Format,
    image_instruction_set: Option<InstructionSet>,
) -> Result<FlashLoader, FileDownloadError> {
    // Create the flash loader
    let mut loader = session.target().flash_loader();

    // Add data from the BIN.
    let mut file = File::open(path).map_err(FileDownloadError::IO)?;

    loader.load_image(session, &mut file, format, image_instruction_set)?;

    Ok(loader)
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory boundaries are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for more options, have a look at [download_file_with_options].
pub fn download_file(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: impl Into<Format>,
) -> Result<(), FileDownloadError> {
    download_file_with_options(session, path, format, DownloadOptions::default())
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory boundaries are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for a simple version without many options, have a look at [download_file].
pub fn download_file_with_options(
    session: &mut Session,
    path: impl AsRef<Path>,
    format: impl Into<Format>,
    options: DownloadOptions,
) -> Result<(), FileDownloadError> {
    let loader = build_loader(session, path, format.into(), None)?;

    loader
        .commit(session, options)
        .map_err(FileDownloadError::Flash)
}

/// Flash data which was extracted from an ELF file.
pub(super) struct ExtractedFlashData<'data> {
    pub(super) section_names: Vec<String>,
    pub(super) address: u32,
    pub(super) data: &'data [u8],
}

impl std::fmt::Debug for ExtractedFlashData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut helper = f.debug_struct("ExtractedFlashData");

        helper
            .field("name", &self.section_names)
            .field("address", &self.address);

        if self.data.len() > 10 {
            helper
                .field("data", &format!("[..] ({} bytes)", self.data.len()))
                .finish()
        } else {
            helper.field("data", &self.data).finish()
        }
    }
}

fn extract_from_elf_inner<'data, T: FileHeader>(
    elf_header: &T,
    binary: ElfFile<'_, T>,
    elf_data: &'data [u8],
    options: &ElfOptions,
) -> Result<Vec<ExtractedFlashData<'data>>, FileDownloadError> {
    let endian = elf_header.endian()?;

    let mut extracted_data = Vec::new();
    for segment in elf_header.program_headers(elf_header.endian()?, elf_data)? {
        // Get the physical address of the segment. The data will be programmed to that location.
        let p_paddr: u64 = segment.p_paddr(endian).into();

        let p_vaddr: u64 = segment.p_vaddr(endian).into();

        let flags = segment.p_flags(endian);

        let segment_data = segment
            .data(endian, elf_data)
            .map_err(|_| FileDownloadError::Object("Failed to access data for an ELF segment."))?;

        let mut elf_section = Vec::new();

        if !segment_data.is_empty() && segment.p_type(endian) == PT_LOAD {
            tracing::info!(
                "Found loadable segment, physical address: {:#010x}, virtual address: {:#010x}, flags: {:#x}",
                p_paddr,
                p_vaddr,
                flags
            );

            let (segment_offset, segment_filesize) = segment.file_range(endian);

            let sector = segment_offset..segment_offset + segment_filesize;

            for section in binary.sections() {
                let (section_offset, section_filesize) = match section.file_range() {
                    Some(range) => range,
                    None => continue,
                };

                if sector.contains_range(&(section_offset..section_offset + section_filesize)) {
                    let name = section.name()?;
                    if options.skip_sections.iter().any(|skip| skip == name) {
                        tracing::info!("Skipping section: {:?}", name);
                        continue;
                    }
                    tracing::info!("Matching section: {:?}", name);

                    #[cfg(feature = "hexdump")]
                    for line in hexdump::hexdump_iter(section.data()?) {
                        tracing::trace!("{}", line);
                    }

                    for (offset, relocation) in section.relocations() {
                        tracing::info!(
                            "Relocation: offset={}, relocation={:?}",
                            offset,
                            relocation
                        );
                    }

                    elf_section.push(name.to_owned());
                }
            }

            if elf_section.is_empty() {
                tracing::info!("Not adding segment, no matching sections found.");
            } else {
                let section_data =
                    &elf_data[segment_offset as usize..][..segment_filesize as usize];

                extracted_data.push(ExtractedFlashData {
                    section_names: elf_section,
                    address: p_paddr as u32,
                    data: section_data,
                });
            }
        }
    }

    Ok(extracted_data)
}

pub(super) fn extract_from_elf<'a>(
    elf_data: &'a [u8],
    options: &ElfOptions,
) -> Result<Vec<ExtractedFlashData<'a>>, FileDownloadError> {
    let file_kind = object::FileKind::parse(elf_data)?;

    match file_kind {
        object::FileKind::Elf32 => {
            let elf_header = FileHeader32::<Endianness>::parse(elf_data)?;
            let binary = object::read::elf::ElfFile::<FileHeader32<Endianness>>::parse(elf_data)?;
            extract_from_elf_inner(elf_header, binary, elf_data, options)
        }
        object::FileKind::Elf64 => {
            let elf_header = FileHeader64::<Endianness>::parse(elf_data)?;
            let binary = object::read::elf::ElfFile::<FileHeader64<Endianness>>::parse(elf_data)?;
            extract_from_elf_inner(elf_header, binary, elf_data, options)
        }
        _ => Err(FileDownloadError::Object("Unsupported file type")),
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::FormatKind;

    #[test]
    fn parse_format() {
        assert_eq!(FormatKind::from_str("hex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Hex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Ihex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("IHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("iHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("IntelHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("intelhex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("intelHex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("Intelhex"), Ok(FormatKind::Hex));
        assert_eq!(FormatKind::from_str("bin"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Bin"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("binary"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Binary"), Ok(FormatKind::Bin));
        assert_eq!(FormatKind::from_str("Elf"), Ok(FormatKind::Elf));
        assert_eq!(FormatKind::from_str("elf"), Ok(FormatKind::Elf));
        assert_eq!(FormatKind::from_str("idf"), Ok(FormatKind::Idf));
        assert_eq!(FormatKind::from_str("espidf"), Ok(FormatKind::Idf));
        assert_eq!(FormatKind::from_str("esp-idf"), Ok(FormatKind::Idf));
        assert_eq!(FormatKind::from_str("ESP-IDF"), Ok(FormatKind::Idf));
        assert_eq!(
            FormatKind::from_str("elfbin"),
            Err("Format 'elfbin' is unknown.".to_string())
        );
        assert_eq!(
            FormatKind::from_str(""),
            Err("Format '' is unknown.".to_string())
        );
        assert_eq!(
            FormatKind::from_str("asdasdf"),
            Err("Format 'asdasdf' is unknown.".to_string())
        );
    }
}
