use object::{
    elf::FileHeader32, elf::PT_LOAD, read::elf::FileHeader, read::elf::ProgramHeader, Bytes,
    Endianness, Object, ObjectSection,
};

use std::{cmp::Ordering, fs::File, path::Path, str::FromStr};

use super::*;
use crate::{config::MemoryRange, session::Session};

use thiserror::Error;

/// Extended options for flashing a binary file.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    pub base_address: Option<u32>,
    /// The number of bytes to skip at the start of the binary file.
    pub skip: u32,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub enum Format {
    /// Marks a file in binary format. This means that the file contains the contents of the flash 1:1.
    /// [BinOptions] can be used to define the location in flash where the file contents should be put at.
    /// Additionally using the same config struct, you can skip the first N bytes of the binary file to have them not put into the flash.
    Bin(BinOptions),
    /// Marks a file in [Intel HEX](https://en.wikipedia.org/wiki/Intel_HEX) format.
    Hex,
    /// Marks a file in the [ELF](https://en.wikipedia.org/wiki/Executable_and_Linkable_Format) format.
    Elf,
}

impl FromStr for Format {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match &s.to_lowercase()[..] {
            "bin" | "binary" => Ok(Format::Bin(BinOptions {
                base_address: None,
                skip: 0,
            })),
            "hex" | "ihex" | "intelhex" => Ok(Format::Hex),
            "elf" => Ok(Format::Elf),
            _ => Err(format!("Format '{}' is unknown.", s)),
        }
    }
}

/// A finite list of all the errors that can occur when flashing a given file.
///
/// This includes corrupt file issues,
/// OS permission issues as well as chip connectivity and memory boundary issues.
#[derive(Debug, Error)]
pub enum FileDownloadError {
    /// An error with the actual flashing procedure has occured.
    ///
    /// This is mostly an error in the communication with the target inflicted by a bad hardware connection or a probe-rs bug.
    #[error("Error while flashing")]
    Flash(#[from] FlashError),
    /// Reading and decoding the IHEX file has failed due to the given error.
    #[error("Could not read ihex format")]
    IhexRead(#[from] ihex::ReaderError),
    /// An IO error has occured while reading the firmware file.
    #[error("I/O error")]
    IO(#[from] std::io::Error),
    /// The given error has occured while reading the object file.
    #[error("Object Error: {0}.")]
    Object(&'static str),
    /// Reading and decoding the given ELF file has resulted in the given error.
    #[error("Could not read ELF file")]
    Elf(#[from] object::read::Error),
    /// No loadable segments were found in the ELF file.
    ///
    /// This is most likely because of a bad linker script.
    #[error("No loadable ELF sections were found.")]
    NoLoadableSegments,
}

/// Options for downloading a file onto a target chip.
#[derive(Default)]
pub struct DownloadOptions<'progress> {
    /// An optional progress reporter which is used if this argument is set to `Some(...)`.
    pub progress: Option<&'progress FlashProgress>,
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
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for more options, have a look at [download_file_with_options].
pub fn download_file<P: AsRef<Path>>(
    session: &mut Session,
    path: P,
    format: Format,
) -> Result<(), FileDownloadError> {
    download_file_with_options(session, path, format, DownloadOptions::default())
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for a simple version without many options, have a look at [download_file].
pub fn download_file_with_options<P: AsRef<Path>>(
    session: &mut Session,
    path: P,
    format: Format,
    options: DownloadOptions<'_>,
) -> Result<(), FileDownloadError> {
    let mut file = match File::open(path.as_ref()) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)),
    };
    let mut buffer = vec![];
    // IMPORTANT: Change this to an actual memory map of a real chip
    let memory_map = session.target().memory_map.clone();

    let mut loader = FlashLoader::new(
        memory_map,
        options.keep_unwritten_bytes,
        session.target().source.clone(),
    );

    match format {
        Format::Bin(options) => loader.load_bin_data(&mut buffer, &mut file, options),
        Format::Elf => loader.load_elf_data(&mut buffer, &mut file),
        Format::Hex => loader.load_hex_data(&mut buffer, &mut file),
    }?;

    loader
        // TODO: hand out chip erase flag
        .commit(
            session,
            options.progress.unwrap_or(&FlashProgress::new(|_| {})),
            options.do_chip_erase,
            options.dry_run,
        )
        .map_err(FileDownloadError::Flash)
}

/// Flash data which was extraced from an ELF file.
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

impl<'data> ExtractedFlashData<'data> {
    /// Create a data slice without tracking its source.
    pub fn from_unknown_source(address: u32, data: &'data [u8]) -> Self {
        Self {
            section_names: vec![],
            address,
            data,
        }
    }

    /// Target address for this flash data.
    pub fn address(&self) -> u32 {
        self.address
    }

    /// Data to be programmed.
    pub fn data(&self) -> &'data [u8] {
        self.data
    }

    /// Split off data from the beginning, and return it. If the offset is
    /// out of bounds, this function will panic.
    pub fn split_off(&mut self, offset: usize) -> ExtractedFlashData<'data> {
        match offset.cmp(&self.data.len()) {
            Ordering::Less => {
                let (first, second) = self.data.split_at(offset);

                let first_address = self.address;

                self.data = second;
                self.address += offset as u32;

                ExtractedFlashData {
                    section_names: self.section_names.clone(),
                    address: first_address,
                    data: first,
                }
            }
            Ordering::Equal => {
                let return_value = ExtractedFlashData {
                    section_names: self.section_names.clone(),
                    address: self.address,
                    data: self.data,
                };

                self.data = &[];

                return_value
            }
            Ordering::Greater => {
                panic!("Offset is out of bounds!");
            }
        }
    }

    /// Length of the contained data.
    pub fn len(&self) -> usize {
        self.data.len()
    }
}

pub(super) fn extract_from_elf<'data>(
    extracted_data: &mut Vec<ExtractedFlashData<'data>>,
    elf_data: &'data [u8],
) -> Result<usize, FileDownloadError> {
    let file_kind = object::FileKind::parse(elf_data)?;

    match file_kind {
        object::FileKind::Elf32 => (),
        _ => return Err(FileDownloadError::Object("Unsupported file type")),
    }

    let elf_header = FileHeader32::<Endianness>::parse(Bytes(elf_data))?;

    let binary = object::read::elf::ElfFile::<FileHeader32<Endianness>>::parse(elf_data)?;

    let endian = elf_header.endian()?;

    let mut extracted_sections = 0;

    for segment in elf_header.program_headers(elf_header.endian()?, Bytes(elf_data))? {
        // Get the physical address of the segment. The data will be programmed to that location.
        let p_paddr: u64 = segment.p_paddr(endian).into();

        let p_vaddr: u64 = segment.p_vaddr(endian).into();

        let flags = segment.p_flags(endian);

        let segment_data = segment
            .data(endian, Bytes(elf_data))
            .map_err(|_| FileDownloadError::Object("Failed to access data for an ELF segment."))?;

        let mut elf_section = Vec::new();

        if !segment_data.is_empty() && segment.p_type(endian) == PT_LOAD {
            log::info!(
                "Found loadable segment, physical address: {:#010x}, virtual address: {:#010x}, flags: {:#x}",
                p_paddr,
                p_vaddr,
                flags
            );

            let (segment_offset, segment_filesize) = segment.file_range(endian);

            let sector: core::ops::Range<u32> =
                segment_offset as u32..segment_offset as u32 + segment_filesize as u32;

            for section in binary.sections() {
                let (section_offset, section_filesize) = match section.file_range() {
                    Some(range) => range,
                    None => continue,
                };

                if sector.contains_range(
                    &(section_offset as u32..section_offset as u32 + section_filesize as u32),
                ) {
                    log::info!("Matching section: {:?}", section.name()?);

                    #[cfg(feature = "hexdump")]
                    for line in hexdump::hexdump_iter(section.data()?) {
                        log::trace!("{}", line);
                    }

                    for (offset, relocation) in section.relocations() {
                        log::info!("Relocation: offset={}, relocation={:?}", offset, relocation);
                    }

                    elf_section.push(section.name()?.to_owned());
                }
            }

            if elf_section.is_empty() {
                log::info!("Not adding segment, no matching sections found.");
            } else {
                let section_data =
                    &elf_data[segment_offset as usize..][..segment_filesize as usize];

                extracted_data.push(ExtractedFlashData {
                    section_names: elf_section,
                    address: p_paddr as u32,
                    data: section_data,
                });

                extracted_sections += 1;
            }
        }
    }

    Ok(extracted_sections)
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{BinOptions, Format};

    #[test]
    fn parse_format() {
        assert_eq!(Format::from_str("hex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("Hex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("Ihex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("IHex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("iHex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("IntelHex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("intelhex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("intelHex"), Ok(Format::Hex));
        assert_eq!(Format::from_str("Intelhex"), Ok(Format::Hex));
        assert_eq!(
            Format::from_str("bin"),
            Ok(Format::Bin(BinOptions {
                base_address: None,
                skip: 0
            }))
        );
        assert_eq!(
            Format::from_str("Bin"),
            Ok(Format::Bin(BinOptions {
                base_address: None,
                skip: 0
            }))
        );
        assert_eq!(
            Format::from_str("binary"),
            Ok(Format::Bin(BinOptions {
                base_address: None,
                skip: 0
            }))
        );
        assert_eq!(
            Format::from_str("Binary"),
            Ok(Format::Bin(BinOptions {
                base_address: None,
                skip: 0
            }))
        );
        assert_eq!(Format::from_str("Elf"), Ok(Format::Elf));
        assert_eq!(Format::from_str("elf"), Ok(Format::Elf));
        assert_eq!(
            Format::from_str("elfbin"),
            Err("Format 'elfbin' is unknown.".to_string())
        );
        assert_eq!(
            Format::from_str(""),
            Err("Format '' is unknown.".to_string())
        );
        assert_eq!(
            Format::from_str("asdasdf"),
            Err("Format 'asdasdf' is unknown.".to_string())
        );
    }
}
