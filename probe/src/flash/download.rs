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
    ) -> Result<(), ()> {
        let mut file = File::open(path).unwrap();
        let mut buffer = vec![];
        // IMPORTANT: Change this to an actual memory map of a real chip
        let mut loader = FlashLoader::new(memory_map, false, false, false);

        match format {
            Format::Bin(options) => self.download_bin(&mut buffer, &mut file, &mut loader, options),
            Format::Elf => self.download_elf(&mut file, &mut loader),
            Format::Hex => self.download_hex(&mut file, &mut loader),
        };

        loader.commit(session);

        Ok(())
    }

    /// Starts the download of a binary file.
    fn download_bin<'b, T: Read + Seek>(self, buffer: &'b mut Vec<u8>, file: &'b mut T, loader: &mut FlashLoader<'_, 'b>, options: BinOptions) -> Result<(), ()> {
        // Skip the specified bytes.
        file.seek(SeekFrom::Start(options.skip as u64));
        
        file.read_to_end(buffer);

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
        );

        Ok(())
    }

    /// Starts the download of a hex file.
    fn download_hex<T: Read + Seek>(self, file: &mut T, loader: &mut FlashLoader) -> Result<(), ()> {
        let mut data = String::new();
        file.read_to_string(&mut data);

        for item in ihex::reader::Reader::new(&data) {
            if let Ok(record) = item {
                println!("{:?}", record);
            } else {
                return Err(());
            }
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
    fn download_elf<T: Read + Seek>(self, file: &mut T, loader: &mut FlashLoader) -> Result<(), ()> {
    // TODO:
    //     elf = ELFBinaryFile(file_obj, self._session.target.memory_map)
    //     for section in elf.sections:
    //         if ((section.type == 'SHT_PROGBITS')
    //                 and ((section.flags & (SH_FLAGS.SHF_ALLOC | SH_FLAGS.SHF_WRITE)) == SH_FLAGS.SHF_ALLOC)
    //                 and (section.length > 0)
    //                 and (section.region.is_flash)):
    //             LOG.debug("Writing section %s", repr(section))
    //             self._loader.add_data(section.start, section.data)
    //         else:
    //             LOG.debug("Skipping section %s", repr(section))
        Ok(())
    }
}