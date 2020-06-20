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
    Bin(BinOptions),
    Hex,
    Elf,
}

/// A finite list of all the errors that can occur when flashing a given file.
///
/// This includes corrupt file issues,
/// OS permission issues as well as chip connectivity and memory boundary issues.
#[derive(Debug, Error)]
pub enum FileDownloadError {
    #[error("Error while flashing")]
    Flash(#[from] FlashError),
    #[error("Could not read ihex format")]
    IhexRead(#[from] ihex::ReaderError),
    #[error("I/O error")]
    IO(#[from] std::io::Error),
    #[error("Object Error: {0}.")]
    Object(&'static str),
    #[error("Could not read ELF file")]
    Elf(#[from] goblin::error::Error),
    #[error("No loadable ELF sections were found.")]
    NoLoadableSegments,
}

/// Options for downloading a file onto a target chip.
#[derive(Default)]
pub struct DownloadOptions<'progress> {
    /// An optional progress reporter which is used if this argument is set to Some(...).
    pub progress: Option<&'progress FlashProgress>,
    /// If `keep_unwritten_bytes` is `true`, erased portions that are not overwritten by the ELF data
    /// are restored afterwards, such that the old contents are untouched.
    pub keep_unwritten_bytes: bool,
}

/// Downloads a file of given `format` at `path` to the flash of the target given in `session`.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If you are looking for more options, have a look at `download_file_with_options`.
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
    let memory_map = session.memory_map().to_vec();
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
            false,
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
    use goblin::elf::program_header::*;

    file.read_to_end(buffer)?;

    let binary = goblin::elf::Elf::parse(&buffer.as_slice())?;
    let mut added_sections = vec![];
    for ph in &binary.program_headers {
        if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
            log::debug!("Found loadable segment.");

            let sector: core::ops::Range<u32> =
                ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

            for sh in &binary.section_headers {
                if sector
                    .contains_range(&(sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32))
                {
                    #[cfg(feature = "hexdump")]
                    for line in hexdump::hexdump_iter(
                        &buffer[sh.sh_offset as usize..][..sh.sh_size as usize],
                    ) {
                        log::trace!("{}", line);
                    }

                    added_sections.push((&binary.shdr_strtab[sh.sh_name], sh.sh_addr, sh.sh_size));
                }
            }

            loader.add_data(
                ph.p_paddr as u32,
                &buffer[ph.p_offset as usize..][..ph.p_filesz as usize],
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
