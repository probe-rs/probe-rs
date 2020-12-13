use goblin::elf64::section_header::SHT_NOBITS;
use ihex::Record::*;

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use super::*;
use crate::{config::MemoryRange, session::Session};

use thiserror::Error;

/// Extended options for flashing a binary file.
#[derive(Debug)]
pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    base_address: Option<u32>,
    /// The number of bytes to skip at the start of the binary file.
    skip: u32,
}

/// A finite list of all the available binary formats probe-rs understands.
#[derive(Debug)]
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
    Elf(#[from] goblin::error::Error),
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
    // IMPORTANT: Change this to an actual memory map of a real chip
    let memory_map = session.memory_map().to_vec();
    let mut loader = FlashLoader::new(
        &memory_map,
        options.keep_unwritten_bytes,
        session.target().source.clone(),
    );

    match format {
        Format::Bin(options) => download_bin(&mut buffer, &mut file, &mut loader, options),
        Format::Elf => download_elf(&mut buffer, &mut file, &mut loader),
        Format::Hex => download_hex(&mut buffer, &mut file, &mut loader),
    }?;

    loader
        // TODO: hand out chip erase flag
        .commit(
            session,
            options.progress.unwrap_or(&FlashProgress::new(|_| {})),
            false,
        )
        .map_err(FileDownloadError::Flash)
}

/// Starts the download of a binary file.
fn download_bin<'buffer, T: Read + Seek>(
    buffer: &'buffer mut Vec<Vec<u8>>,
    file: &'buffer mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
    options: BinOptions,
) -> Result<(), FileDownloadError> {
    let mut file_buffer = Vec::new();

    // Skip the specified bytes.
    file.seek(SeekFrom::Start(u64::from(options.skip)))?;

    file.read_to_end(&mut file_buffer)?;

    buffer.push(file_buffer);

    loader.add_data(
        if let Some(address) = options.base_address {
            address
        } else {
            // If no base address is specified use the start of the boot memory.
            // TODO: Implement this as soon as we know targets.
            0
        },
        buffer.last().unwrap(),
    )?;

    Ok(())
}

/// Starts the download of a hex file.
fn download_hex<'buffer, T: Read + Seek>(
    data_buffer: &'buffer mut Vec<Vec<u8>>,
    file: &mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
) -> Result<(), FileDownloadError> {
    let mut _extended_segment_address = 0;
    let mut extended_linear_address = 0;

    let mut data = String::new();
    file.read_to_string(&mut data)?;

    let mut offsets: Vec<(u32, usize)> = Vec::new();

    for record in ihex::Reader::new(&data) {
        let record = record?;
        match record {
            Data { offset, value } => {
                let offset = extended_linear_address | offset as u32;

                let index = data_buffer.len();
                data_buffer.push(value);

                offsets.push((offset, index))
            }
            EndOfFile => return Ok(()),
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
    for (offset, data_index) in offsets {
        loader.add_data(offset, &data_buffer[data_index])?;
    }
    Ok(())
}

pub struct ExtractedFlashData<'data> {
    name: Vec<String>,
    address: u32,
    data: &'data [u8],
}

impl std::fmt::Debug for ExtractedFlashData<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut helper = f.debug_struct("ExtractedFlashData");

        helper
            .field("name", &self.name)
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
    pub fn from_unknown_source(address: u32, data: &'data [u8]) -> Self {
        Self {
            name: vec![],
            address,
            data,
        }
    }

    pub fn address(&self) -> u32 {
        self.address
    }

    pub fn data(&self) -> &'data [u8] {
        self.data
    }

    pub fn split_at_beginning(&mut self, offset: usize) -> ExtractedFlashData<'data> {
        if offset < self.data.len() {
            let (first, second) = self.data.split_at(offset);

            let first_address = self.address;

            self.data = second;
            self.address += offset as u32;

            ExtractedFlashData {
                name: self.name.clone(),
                address: first_address,
                data: first,
            }
        } else if offset == self.data.len() {
            let return_value = ExtractedFlashData {
                name: self.name.clone(),
                address: self.address,
                data: self.data,
            };

            self.data = &[];

            return_value
        } else {
            unimplemented!("TOOD: Handle out of bounds");
        }
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }
}

/// Starts the download of a elf file.
pub fn download_elf<'buffer, T: Read>(
    buffer: &'buffer mut Vec<Vec<u8>>,
    file: &mut T,
    loader: &mut FlashLoader<'_, 'buffer>,
) -> Result<(), FileDownloadError> {
    buffer.push(Vec::new());

    let elf_buffer = buffer.last_mut().unwrap();

    file.read_to_end(elf_buffer)?;

    let mut extracted_data = Vec::new();

    let num_sections = extract_from_elf(&mut extracted_data, elf_buffer)?;

    if num_sections == 0 {
        log::warn!("No loadable segments were found in the ELF file.");
        return Err(FileDownloadError::NoLoadableSegments);
    }

    log::info!("Found {} loadable sections:", num_sections);

    for section in &extracted_data {
        let source = if section.name.is_empty() {
            "Unknown".to_string()
        } else if section.name.len() == 1 {
            section.name[0].to_owned()
        } else {
            "Multiple sections".to_owned()
        };

        log::info!(
            "    {} at {:08X?} ({} byte{})",
            source,
            section.address,
            section.data.len(),
            if section.data.len() == 1 { "" } else { "s" }
        );
    }

    for data in extracted_data {
        loader.add_section(data)?;
    }

    Ok(())
}

fn extract_from_elf<'data, 'elf: 'data>(
    extracted_data: &mut Vec<ExtractedFlashData<'data>>,
    elf_data: &'data [u8],
) -> Result<usize, FileDownloadError> {
    use goblin::elf::program_header::*;

    let binary = goblin::elf::Elf::parse(&elf_data)?;

    let mut extracted_sections = 0;

    for ph in &binary.program_headers {
        if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
            log::info!(
                "Found loadable segment, target address: {:#10x}",
                ph.p_paddr
            );

            // The file section is the part of the ELF which
            // contains the data for the current program header.
            //
            // This is the data that should be loaded into memory.
            let file_section: core::ops::Range<u32> =
                ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

            let mut elf_section = Vec::new();

            for sh in &binary.section_headers {
                if file_section
                    .contains_range(&(sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32))
                {
                    if sh.sh_size > 0 && (sh.sh_type & SHT_NOBITS) == 0 {
                        log::info!(
                            "ELF Section:     {}",
                            binary.shdr_strtab[sh.sh_name].to_owned()
                        );
                        log::info!("\tFile Range:    {:x?}", sh.file_range());
                        log::info!("\tVirtual Range: {:x?}", sh.vm_range());

                        elf_section.push(binary.shdr_strtab[sh.sh_name].to_owned());
                    } else {
                        log::debug!(
                            "ELF Section: {} is empty!",
                            binary.shdr_strtab[sh.sh_name].to_owned()
                        );
                    }

                    #[cfg(feature = "hexdump")]
                    for line in hexdump::hexdump_iter(
                        &buffer[sh.sh_offset as usize..][..sh.sh_size as usize],
                    ) {
                        log::trace!("{}", line);
                    }
                }
            }

            let section_data = &elf_data[ph.p_offset as usize..][..ph.p_filesz as usize];

            extracted_data.push(ExtractedFlashData {
                name: elf_section,
                address: ph.p_paddr as u32,
                data: section_data,
            });

            extracted_sections += 1;
        }
    }

    Ok(extracted_sections)
}
