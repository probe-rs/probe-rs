use ihex;
use ihex::record::Record::*;

use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

use super::*;
use crate::{
    config::{MemoryRange, MemoryRegion},
    session::Session,
};

use thiserror::Error;

pub struct BinOptions {
    /// The address in memory where the binary will be put at.
    base_address: Option<u32>,
    /// The number of bytes to skip at the start of the binary file.
    skip: u32,
}

pub enum Format {
    Bin(BinOptions),
    Hex,
    Elf,
}

#[derive(Debug, Error)]
pub enum FileDownloadError {
    #[error("{0}")]
    Flash(#[from] FlashError),
    #[error("{0}")]
    IhexRead(#[from] ihex::reader::ReaderError),
    #[error("{0}")]
    IO(#[from] std::io::Error),
    #[error("Object Error: {0}.")]
    Object(&'static str),
}

/// Downloads a file of given `format` at `path` to the flash with progress reporting.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If `keep_unwritten_bytes` is `true`, erased portions that are not overwritten by the ELF data
/// are restored afterwards, such that the old contents are untouched.
///
/// If no progress reporting is desired, have a look at `flashing::download_file()`.
pub fn download_file_with_progress_reporting(
    session: &Session,
    path: &Path,
    format: Format,
    memory_map: &[MemoryRegion],
    keep_unwritten_bytes: bool,
    progress: &FlashProgress,
) -> Result<(), FileDownloadError> {
    download_file_internal(
        session,
        path,
        format,
        memory_map,
        keep_unwritten_bytes,
        progress,
    )
}

/// Downloads a file of given `format` at `path` to the flash.
///
/// This will ensure that memory bounderies are honored and does unlocking, erasing and programming of the flash for you.
///
/// If `keep_unwritten_bytes` is `true`, erased portions that are not overwritten by the ELF data
/// are restored afterwards, such that the old contents are untouched.
///
/// If progress reporting is desired, have a look at `flashing::download_file_with_progress_reporting()`.
pub fn download_file(
    session: &Session,
    path: &Path,
    format: Format,
    keep_unwritten_bytes: bool,
    memory_map: &[MemoryRegion],
) -> Result<(), FileDownloadError> {
    download_file_internal(
        session,
        path,
        format,
        memory_map,
        keep_unwritten_bytes,
        &FlashProgress::new(|_| {}),
    )
}

/// Downloads a file at `path` into flash.
///
/// If `keep_unwritten_bytes` is `true`, erased portions that are not overwritten by the ELF data
/// are restored afterwards, such that the old contents are untouched.
fn download_file_internal(
    session: &Session,
    path: &Path,
    format: Format,
    memory_map: &[MemoryRegion],
    keep_unwritten_bytes: bool,
    progress: &FlashProgress,
) -> Result<(), FileDownloadError> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) => return Err(FileDownloadError::IO(e)),
    };
    let mut buffer = vec![];
    let mut buffer_vec = vec![];
    // IMPORTANT: Change this to an actual memory map of a real chip
    let mut loader = FlashLoader::new(memory_map, false);

    match format {
        Format::Bin(options) => download_bin(&mut buffer, &mut file, &mut loader, options),
        Format::Elf => download_elf(&mut buffer, &mut file, &mut loader),
        Format::Hex => download_hex(&mut buffer_vec, &mut file, &mut loader),
    }?;

    loader
        // TODO: hand out chip erase flag
        .commit(session, progress, false)
        .map_err(FileDownloadError::Flash)
}

/// Starts the download of a binary file.
fn download_bin<'b, T: Read + Seek>(
    buffer: &'b mut Vec<u8>,
    file: &'b mut T,
    loader: &mut FlashLoader<'_, 'b>,
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
fn download_hex<'b, T: Read + Seek>(
    buffer: &'b mut Vec<(u32, Vec<u8>)>,
    file: &mut T,
    loader: &mut FlashLoader<'_, 'b>,
) -> Result<(), FileDownloadError> {
    let mut _extended_segment_address = 0;
    let mut extended_linear_address = 0;

    let mut data = String::new();
    file.read_to_string(&mut data)?;

    for record in ihex::reader::Reader::new(&data) {
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
fn download_elf<'b, T: Read + Seek>(
    buffer: &'b mut Vec<u8>,
    file: &'b mut T,
    loader: &mut FlashLoader<'_, 'b>,
) -> Result<(), FileDownloadError> {
    file.read_to_end(buffer)?;

    use goblin::elf::program_header::*;

    if let Ok(binary) = goblin::elf::Elf::parse(&buffer.as_slice()) {
        for ph in &binary.program_headers {
            if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                log::debug!("Found loadable segment containing:");

                let sector: core::ops::Range<u32> =
                    ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

                for sh in &binary.section_headers {
                    if sector.contains_range(
                        &(sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32),
                    ) {
                        log::debug!("{:?}", &binary.shdr_strtab[sh.sh_name]);
                        #[cfg(feature = "hexdump")]
                        for line in hexdump::hexdump_iter(
                            &buffer[sh.sh_offset as usize..][..sh.sh_size as usize],
                        ) {
                            log::trace!("{}", line);
                        }
                    }
                }

                loader.add_data(
                    ph.p_paddr as u32,
                    &buffer[ph.p_offset as usize..][..ph.p_filesz as usize],
                )?;
            }
        }
    }
    Ok(())
}
