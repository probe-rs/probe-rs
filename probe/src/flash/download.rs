use crate::session::Session;
use std::path::Path;
use std::io::{ Read, Seek, SeekFrom };
use std::fs::File;
use ihex;

use super::*;

pub struct BinOptions {
    /// Memory address at which to program the binary data. If not set, the base
    /// of the boot memory will be used.
    base_address: Option<u32>,
    /// Number of bytes to skip at the start of the binary file. Does not affect the
    /// base address.
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

/// This struct and impl bundle functionality to start the `Downloader` which then will flash
/// the given data to the flash of the target.
/// 
/// Supported file formats are:
/// - Binary (.bin)
/// - Intel Hex (.hex)
/// - ELF (.elf or .axf)
pub struct FileDownloader;

impl<'a> FileDownloader {
    pub fn new() -> Self {
        Self {
        }
    }

    /// Downloads a file at `path` into flash.
    pub fn download_file(
        self,
        session: &mut Session,
        path: &Path,
        format: Format,
        memory_map: &Vec<MemoryRegion>
    ) -> Result<(), FileDownloadError> {
        let mut file = File::open(path).unwrap();
        let mut buffer = vec![];
        // IMPORTANT: Change this to an actual memory map of a real chip
        let mut loader = FlashLoader::new(memory_map, false, false, false);

        match format {
            Format::Bin(options) => self.download_bin(&mut buffer, &mut file, &mut loader, options),
            Format::Elf => self.download_elf(&mut buffer, &mut file, &mut loader),
            Format::Hex => self.download_hex(&mut file, &mut loader),
        }?;

        loader.commit(session);

        Ok(())
    }

    /// Starts the download of a binary file.
    fn download_bin<'b, T: Read + Seek>(
        self,
        buffer: &'b mut Vec<u8>,
        file: &'b mut T,
        loader: &mut FlashLoader<'_, 'b>,
        options: BinOptions
    ) -> Result<(), FileDownloadError> {
        // Skip the specified bytes.
        file.seek(SeekFrom::Start(options.skip as u64))?;
        
        file.read_to_end(buffer)?;

        loader.add_data(
            if let Some(address) = options.base_address {
                address
            } else {
                // If no base address is specified use the start of the boot memory.
                // TODO: Implement this as soon as we know targets.
                // self._session.target.memory_map.get_boot_memory().start
                0
            },
            buffer.as_slice()
        )?;

        Ok(())
    }

    /// Starts the download of a hex file.
    fn download_hex<T: Read + Seek>(self, file: &mut T, _loader: &mut FlashLoader) -> Result<(), FileDownloadError> {
        let mut data = String::new();
        file.read_to_string(&mut data)?;

        for item in ihex::reader::Reader::new(&data) {
            println!("{:?}", item?);
        }
        Ok(())

        // hexfile = IntelHex(file_obj)
        // addresses = hexfile.addresses()
        // addresses.sort()

        // data_list = list(ranges(addresses))
        // for start, end in data_list:
        //     size = end - start + 1
        //     data = list(hexfile.tobinarray(start=start, size=size))
        //     self._loader.add_data(start, data)
    }
        
    /// Starts the download of a elf file.
    fn download_elf<'b, T: Read + Seek>(
        self,
        buffer: &'b mut Vec<u8>,
        file: &'b mut T,
        loader: &mut FlashLoader<'_, 'b>
    ) -> Result<(), FileDownloadError> {
        file.read_to_end(buffer)?;

        use goblin::elf::program_header::*;

        match goblin::elf::Elf::parse(&buffer.as_slice()) {
            Ok(binary) => {
                for ph in &binary.program_headers {
                    if ph.p_type == PT_LOAD && ph.p_filesz > 0 {
                        log::debug!("Found loadable segment containing:");
                        
                        let sector: core::ops::Range<u32> = ph.p_offset as u32..ph.p_offset as u32 + ph.p_filesz as u32;

                        for sh in &binary.section_headers {
                            if sector.contains_range(&(sh.sh_offset as u32..sh.sh_offset as u32 + sh.sh_size as u32)) {
                                log::debug!("{:?}", &binary.shdr_strtab[sh.sh_name]);
                            }
                        }


                        loader.add_data(ph.p_paddr as u32, &buffer[ph.p_offset as usize..][..ph.p_filesz as usize])?;
                    }
                }
            },
            Err(_) => ()
        }
        Ok(())
    }
}