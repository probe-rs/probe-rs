use ihex::Record::*;
use object::{
    elf::FileHeader32, read::elf::FileHeader, read::elf::ProgramHeader, Bytes, Endianness, Object,
    ObjectSection,
};

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    str::FromStr,
};

use super::*;
use crate::{config::MemoryRange, session::Session};

use thiserror::Error;

/// Extended options for flashing a binary file.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    pub base_address: Option<u32>,
    /// The number of bytes to skip at the start of the binary file.
    pub skip: u32,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
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
pub fn download_file(
    session: &mut Session,
    path: &Path,
    format: Format,
) -> Result<(), FileDownloadError> {
    download_file_with_options(session, path, format, DownloadOptions::default())
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for a simple version without many options, have a look at [download_file].
pub fn download_file_with_options(
    session: &mut Session,
    path: &Path,
    format: Format,
    options: DownloadOptions<'_>,
) -> Result<(), FileDownloadError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)),
    };
    let mut buffer = vec![];
    let mut buffer_vec = vec![];
    // IMPORTANT: Change this to an actual memory map of a real chip
    let memory_map = session.target().memory_map.clone();
    let mut loader = FlashLoader::new(&memory_map, options.keep_unwritten_bytes);

    match format {
        Format::Bin(options) => download_bin(&mut buffer, &mut file, &mut loader, options),
        Format::Elf => download_elf(&mut buffer, &mut file, &mut loader),
        Format::Hex => download_hex(&mut buffer_vec, &mut file, &mut loader),
    }?;

    loader
        // TODO: hand out chip erase flag
        .commit(
            session,
            options.progress.unwrap_or(&FlashProgress::new(|_| {})),
            options.do_chip_erase,
        )
        .map_err(FileDownloadError::Flash)
}

/// Starts the download of a binary file.
fn download_bin<'buffer, T: Read + Seek>(
    buffer: &'buffer mut Vec<u8>,
    file: &'buffer mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
    options: BinOptions,
) -> Result<(), FileDownloadError> {
    // Skip the specified bytes.
    file.seek(SeekFrom::Start(u64::from(options.skip)))?;

    file.read_to_end(buffer)?;

    loader.add_data(
        if let Some(address) = options.base_address {
            address
        } else {
            // If no base address is specified use the start of the boot memory.
            // TODO: Implement this as soon as we know targets.
            0
        },
        buffer.as_slice(),
    )?;

    Ok(())
}

/// Starts the download of a hex file.
fn download_hex<'buffer, T: Read + Seek>(
    buffer: &'buffer mut Vec<(u32, Vec<u8>)>,
    file: &mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
) -> Result<(), FileDownloadError> {
    let mut _extended_segment_address = 0;
    let mut extended_linear_address = 0;

    let mut data = String::new();
    file.read_to_string(&mut data)?;

    for record in ihex::Reader::new(&data) {
        let record = record?;
        match record {
            Data { offset, value } => {
                let offset = extended_linear_address | offset as u32;
                buffer.push((offset, value));
            }
            EndOfFile => (),
            ExtendedSegmentAddress(address) => {
                _extended_segment_address = address * 16;
            }
            StartSegmentAddress { .. } => (),
            ExtendedLinearAddress(address) => {
                extended_linear_address = (address as u32) << 16;
            }
            StartLinearAddress(_) => (),
        };
    }
    for (offset, data) in buffer {
        loader.add_data(*offset, data.as_slice())?;
    }
    Ok(())
}

/// Starts the download of a elf file.
fn download_elf<'buffer, T: Read + Seek>(
    buffer: &'buffer mut Vec<u8>,
    file: &'buffer mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
) -> Result<(), FileDownloadError> {
    file.read_to_end(buffer)?;

    let file_kind = object::FileKind::parse(buffer)?;

    match file_kind {
        object::FileKind::Elf32 => (),
        _ => return Err(FileDownloadError::Object("Unsupported file type")),
    }

    let elf_header = FileHeader32::<Endianness>::parse(Bytes(buffer))?;

    let binary = object::read::elf::ElfFile::<FileHeader32<Endianness>>::parse(buffer)?;

    let endian = elf_header.endian()?;

    let mut added_sections = vec![];

    for segment in elf_header.program_headers(elf_header.endian()?, Bytes(buffer))? {
        // Get the physical address of the segment. The data will be programmed to that location.
        let p_paddr: u64 = segment.p_paddr(endian).into();

        let segment_data = segment
            .data(endian, Bytes(buffer))
            .map_err(|_| FileDownloadError::Object("Failed to access data for an ELF segment."))?;

        if !segment_data.is_empty() {
            log::info!("Found loadable segment, address: {:#010x}", p_paddr);

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

                    added_sections.push((
                        section.name()?.to_owned(),
                        section.address(),
                        section.size(),
                    ));
                }
            }

            loader.add_data(
                p_paddr as u32,
                &buffer
                    [segment_offset as usize..segment_offset as usize + segment_filesize as usize],
            )?;
        }
    }
    if added_sections.is_empty() {
        log::warn!("No loadable segments were found in the ELF file.");
        Err(FileDownloadError::NoLoadableSegments)
    } else {
        log::info!("Found {} loadable sections:", added_sections.len());
        for section in added_sections {
            log::info!(
                "    {} at {:08X?} ({} byte{})",
                section.0,
                section.1,
                section.2,
                if section.2 == 1 { "" } else { "0" }
            );
        }
        Ok(())
    }
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
