use crate::session::Session;
use ihex;
use ihex::record::Record::*;
use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use super::*;
use crate::config::memory::{MemoryRange, MemoryRegion};

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

#[derive(Debug)]
pub enum FileDownloadError {
    FlashLoader(FlashLoaderError),
    IhexRead(ihex::reader::ReaderError),
    IO(std::io::Error),
    Object(&'static str),
}

impl Error for FileDownloadError {}

impl fmt::Display for FileDownloadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use FileDownloadError::*;

        match self {
            FlashLoader(ref e) => e.fmt(f),
            IhexRead(ref e) => e.fmt(f),
            IO(ref e) => e.fmt(f),
            Object(ref s) => write!(f, "Object Error: {}.", s),
        }
    }
}

impl From<FlashLoaderError> for FileDownloadError {
    fn from(error: FlashLoaderError) -> FileDownloadError {
        FileDownloadError::FlashLoader(error)
    }
}

impl From<ihex::reader::ReaderError> for FileDownloadError {
    fn from(error: ihex::reader::ReaderError) -> FileDownloadError {
        FileDownloadError::IhexRead(error)
    }
}

impl From<std::io::Error> for FileDownloadError {
    fn from(error: std::io::Error) -> FileDownloadError {
        FileDownloadError::IO(error)
    }
}

impl From<&'static str> for FileDownloadError {
    fn from(error: &'static str) -> FileDownloadError {
        FileDownloadError::Object(error)
    }
}

/// Downloads a file at `path` into flash.
pub fn download_file_with_progress_reporting(
    session: &mut Session,
    path: &Path,
    format: Format,
    memory_map: &[MemoryRegion],
    progress: &FlashProgress,
) -> Result<(), FileDownloadError> {
    download_file_internal(session, path, format, memory_map, progress)
}

/// Downloads a file at `path` into flash.
pub fn download_file(
    session: &mut Session,
    path: &Path,
    format: Format,
    memory_map: &[MemoryRegion],
) -> Result<(), FileDownloadError> {
    download_file_internal(
        session,
        path,
        format,
        memory_map,
        &FlashProgress::new(|_| {}),
    )
}

/// Downloads a file at `path` into flash.
fn download_file_internal(
    session: &mut Session,
    path: &Path,
    format: Format,
    memory_map: &[MemoryRegion],
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
        .map_err(FileDownloadError::FlashLoader)
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
