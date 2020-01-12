use super::flasher::{Flasher, FlasherError};
use super::FlashProgress;
use crate::config::memory::{PageInfo, SectorInfo};

/// A struct to hold all the information about one page of flash.
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct FlashPage {
    #[derivative(Debug(format_with = "fmt_hex"))]
    address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    size: u32,
    #[derivative(Debug(format_with = "fmt"))]
    data: Vec<u8>,
}

fn fmt(data: &[u8], f: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
    write!(f, "[{} bytes]", data.len())
}

fn fmt_hex<T: std::fmt::LowerHex>(
    data: &T,
    f: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    write!(f, "0x{:08x}", data)
}

impl FlashPage {
    pub fn new(page_info: &PageInfo) -> Self {
        Self {
            address: page_info.base_address,
            size: page_info.size,
            data: vec![],
        }
    }
}

/// A struct to hold all the information about one Sector in flash.
#[derive(Derivative)]
#[derivative(Debug, Clone)]
pub struct FlashSector {
    #[derivative(Debug(format_with = "fmt_hex"))]
    address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    size: u32,
    page_size: u32,
    pages: Vec<FlashPage>,
}

impl FlashSector {
    /// Creates a new empty flash sector form a `SectorInfo`.
    pub fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
            page_size: sector_info.page_size,
            pages: vec![],
        }
    }

    /// Adds a new `FlashPage` to the `FlashSector`.
    pub fn add_page(&mut self, page: FlashPage) -> Result<(), FlashBuilderError> {
        // If the pages do not align nicely within the sector, return an error.
        if self.page_size != page.size {
            return Err(FlashBuilderError::PageSizeDoesNotMatch(
                page.size,
                self.page_size,
            ));
        }

        // Determine the maximal amout of pages in the sector.
        let max_page_count = (self.size / page.size) as usize;

        // Make sure we haven't reached the sectors maximum capacity yet.
        if self.pages.len() < max_page_count {
            // Add a page and keep the pages sorted.
            self.pages.push(page);
            self.pages.sort_by_key(|p| p.address);
        } else {
            return Err(FlashBuilderError::MaxPageCountExceeded(max_page_count));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FlashWriteData<'a> {
    pub address: u32,
    pub data: &'a [u8],
}

impl<'a> FlashWriteData<'a> {
    pub fn new(address: u32, data: &'a [u8]) -> Self {
        Self { address, data }
    }
}

#[derive(Default)]
pub struct FlashBuilder<'a> {
    flash_write_data: Vec<FlashWriteData<'a>>,
    buffered_data_size: usize,
    enable_double_buffering: bool,
}

#[derive(Debug)]
pub enum FlashBuilderError {
    AddressBeforeFlashStart(u32),   // Contains faulty address.
    DataOverlap(u32),               // Contains faulty address.
    InvalidFlashAddress(u32),       // Contains faulty address.
    DuplicateDataEntry(u32),        // There is two entries for data at the same address.
    PageSizeDoesNotMatch(u32, u32), // The flash sector size is not a multiple of the flash page size.
    MaxPageCountExceeded(usize),
    ProgramPage(u32, u32),
    Flasher(FlasherError),
}

impl From<FlasherError> for FlashBuilderError {
    fn from(error: FlasherError) -> Self {
        FlashBuilderError::Flasher(error)
    }
}

type R = Result<(), FlashBuilderError>;

impl<'a> FlashBuilder<'a> {
    /// Creates a new `FlashBuilder` with empty data.
    pub fn new() -> Self {
        Self {
            flash_write_data: vec![],
            buffered_data_size: 0,
            enable_double_buffering: false,
        }
    }

    /// Iterate over all pages in an array of `FlashSector`s.
    pub fn pages(sectors: &[FlashSector]) -> Vec<&FlashPage> {
        sectors.iter().map(|s| &s.pages).flatten().collect()
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub fn add_data(&mut self, address: u32, data: &'a [u8]) -> Result<(), FlashBuilderError> {
        // Add the operation to the sorted data list.
        match self
            .flash_write_data
            .binary_search_by_key(&address, |&v| v.address)
        {
            // If it already is present in the list, return an error.
            Ok(_) => return Err(FlashBuilderError::DuplicateDataEntry(address)),
            // Add it to the list if it is not present yet.
            Err(position) => self
                .flash_write_data
                .insert(position, FlashWriteData::new(address, data)),
        }
        self.buffered_data_size += data.len();

        // Verify that the data list does not have overlapping addresses.
        // We assume that we made sure the list of data write commands is always ordered by address.
        // Thus we only have to check subsequent flash write commands for overlap.
        let mut previous_operation: Option<&FlashWriteData> = None;
        for operation in &self.flash_write_data {
            if let Some(previous) = previous_operation {
                if previous.address + previous.data.len() as u32 > operation.address {
                    return Err(FlashBuilderError::DataOverlap(operation.address));
                }
            }
            previous_operation = Some(operation);
        }
        Ok(())
    }

    /// Program a binary into the flash.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    pub fn program(
        &self,
        mut flash: Flasher,
        mut do_chip_erase: bool,
        restore_unwritten_bytes: bool,
        progress: &FlashProgress,
    ) -> Result<(), FlashBuilderError> {
        if self.flash_write_data.is_empty() {
            // Nothing to do.
            return Ok(());
        }

        // Convert the list of flash operations into flash sectors and pages.
        let sectors = self.build_sectors_and_pages(&mut flash, restore_unwritten_bytes)?;

        let num_pages = sectors.iter().map(|s| s.pages.len()).sum();
        let sizes = sectors.first().map(|s| (s.size, s.page_size));
        let (sector_size, page_size) = sizes.unwrap_or((0, 0));

        let sector_size: u32 = sectors.iter().map(|s| s.size).sum();

        progress.initialized(num_pages, sector_size as usize, page_size);

        // Check if there is even sectors to flash.
        if sectors.is_empty() || sectors[0].pages.is_empty() {
            // Nothing to do.
            return Ok(());
        }

        // If the flash algo doesn't support erase all, disable chip erase.
        if flash.flash_algorithm().pc_erase_all.is_none() {
            do_chip_erase = false;
        }

        log::debug!("Full Chip Erase enabled: {:?}", do_chip_erase);
        log::debug!(
            "Double Buffering enabled: {:?}",
            self.enable_double_buffering
        );

        // Erase all necessary sectors.
        progress.started_erasing();

        if do_chip_erase {
            self.chip_erase(&mut flash, &sectors, progress)?;
        } else {
            self.sector_erase(&mut flash, &sectors, progress)?;
        }

        // Flash all necessary pages.
        progress.started_flashing();

        if flash.double_buffering_supported() && self.enable_double_buffering {
            self.program_double_buffer(&mut flash, &sectors, progress)?;
        } else {
            self.program_simple(&mut flash, &sectors, progress)?;
        };

        Ok(())
    }

    /// Layouts an entire flash memory.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    fn build_sectors_and_pages(
        &self,
        flash: &mut Flasher,
        restore_unwritten_bytes: bool,
    ) -> Result<Vec<FlashSector>, FlashBuilderError> {
        let mut sectors: Vec<FlashSector> = Vec::new();

        for op in &self.flash_write_data {
            let mut pos = 0;

            while pos < op.data.len() {
                // Check if the operation is in another sector.
                let flash_address = op.address + pos as u32;

                log::trace!("Checking sector for address {:#08x}", flash_address);

                if let Some(sector) = sectors.last_mut() {
                    // If the address is not in the sector, add a new sector.
                    if flash_address >= sector.address + sector.size {
                        let sector_info = flash.sector_info(flash_address);
                        if let Some(sector_info) = sector_info {
                            let new_sector = FlashSector::new(&sector_info);
                            sectors.push(new_sector);
                            log::trace!(
                                "Added Sector (0x{:08x}..0x{:08x})",
                                sector_info.base_address,
                                sector_info.base_address + sector_info.size
                            );
                        } else {
                            return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                        }
                        continue;
                    } else if let Some(page) = sector.pages.last_mut() {
                        // If the current page does not contain the address.
                        if flash_address >= page.address + page.size {
                            // Fill any gap at the end of the current page before switching to a new page.
                            Self::fill_page(flash, page, restore_unwritten_bytes)?;

                            let page_info = flash.flash_algorithm().page_info(flash_address);
                            if let Some(page_info) = page_info {
                                let new_page = FlashPage::new(&page_info);
                                sector.add_page(new_page)?;
                                log::trace!(
                                    "Added Page (0x{:08x}..0x{:08x})",
                                    page_info.base_address,
                                    page_info.base_address + page_info.size
                                );
                            } else {
                                return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                            }
                            continue;
                        } else {
                            let space_left_in_page = page.size - page.data.len() as u32;
                            let space_left_in_data = op.data.len() - pos;
                            let amount =
                                usize::min(space_left_in_page as usize, space_left_in_data);

                            page.data.extend(&op.data[pos..pos + amount]);
                            log::trace!("Added {} bytes to current page", amount);
                            pos += amount;
                        }
                    } else {
                        // If no page is on the sector yet.
                        let page_info = flash.flash_algorithm().page_info(flash_address);
                        if let Some(page_info) = page_info {
                            let new_page = FlashPage::new(&page_info);
                            sector.add_page(new_page.clone())?;
                            log::trace!(
                                "Added Page (0x{:08x}..0x{:08x})",
                                page_info.base_address,
                                page_info.base_address + page_info.size
                            );
                        } else {
                            return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                        }
                        continue;
                    }
                } else {
                    // If no sector exists, create a new one.
                    log::trace!("Trying to create a new sector");
                    let sector_info = flash.sector_info(flash_address);

                    if let Some(sector_info) = sector_info {
                        let new_sector = FlashSector::new(&sector_info);
                        sectors.push(new_sector);
                        log::debug!(
                            "Added Sector (0x{:08x}..0x{:08x})",
                            sector_info.base_address,
                            sector_info.base_address + sector_info.size
                        );
                    } else {
                        return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                    }
                    continue;
                }
            }
        }

        // Fill the page gap if there is one.
        if let Some(sector) = sectors.last_mut() {
            if let Some(page) = sector.pages.last_mut() {
                Self::fill_page(flash, page, restore_unwritten_bytes)?;
            }
        }

        log::debug!("Sectors are:");
        for sector in &sectors {
            log::debug!("{:#?}", sector);
        }

        Ok(sectors)
    }

    /// Fills all the bytes of `current_page`.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of the page,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the page is programmed.
    fn fill_page(
        flash: &mut Flasher,
        current_page: &mut FlashPage,
        restore_unwritten_bytes: bool,
    ) -> Result<(), FlashBuilderError> {
        // The remaining bytes to be filled in at the end of the page.
        let remaining_bytes = current_page.size as usize - current_page.data.len();
        if current_page.data.len() != current_page.size as usize {
            let address_remaining_start = current_page.address + current_page.data.len() as u32;

            // Fill up the page with current page bytes until it's full.
            let old_data = if restore_unwritten_bytes {
                // Read all the remaining old bytes from flash to restore them later.
                let mut data = vec![0; remaining_bytes];
                flash.run_verify(|active| {
                    active.read_block8(address_remaining_start, data.as_mut_slice())
                })?;
                data
            } else {
                // Set all the remaining bytes to their default erased value.
                vec![flash.flash_algorithm().flash_properties.erased_byte_value; remaining_bytes]
            };
            current_page.data.extend(old_data);
        }
        Ok(())
    }

    // Erase the entire chip.
    fn chip_erase(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashBuilderError> {
        let mut t = std::time::Instant::now();
        let result = flash
            .run_erase(|active| active.erase_all())
            .map_err(From::from);
        for sector in sectors {
            progress.sector_erased(sector.page_size, t.elapsed().as_millis());
            t = std::time::Instant::now();
        }
        progress.finished_erasing();
        result
    }

    /// Program all sectors in `sectors` by first performing a chip erase.
    fn program_simple(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashBuilderError> {
        let mut t = std::time::Instant::now();
        let result = flash.run_program(|active| {
            for page in Self::pages(sectors) {
                active.program_page(page.address, page.data.as_slice())?;
                progress.page_programmed(page.size, t.elapsed().as_millis());
                t = std::time::Instant::now();
            }
            Ok(())
        });
        progress.finished_programming();
        result
    }

    /// Perform an erase of all sectors given in `sectors` which contain pages.
    fn sector_erase(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashBuilderError> {
        let mut t = std::time::Instant::now();
        let r: R = flash.run_erase(|active| {
            for sector in sectors {
                if !sector.pages.is_empty() {
                    active.erase_sector(sector.address)?;
                    progress.sector_erased(sector.size, t.elapsed().as_millis());
                    t = std::time::Instant::now();
                }
            }
            Ok(())
        });
        r?;
        progress.finished_erasing();
        Ok(())
    }

    /// Flash a program using double buffering.
    ///
    /// UNTESTED
    fn program_double_buffer(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
        progress: &FlashProgress,
    ) -> Result<(), FlashBuilderError> {
        let mut current_buf = 0;
        let mut t = std::time::Instant::now();
        let result = flash.run_program(|active| {
            for page in Self::pages(sectors) {
                // At the start of each loop cycle load the next page buffer into RAM.
                active.load_page_buffer(page.address, page.data.as_slice(), current_buf)?;

                // Then wait for the active RAM -> Flash copy process to finish.
                // Also check if it finished properly. If it didn't, return an error.
                let result = active.wait_for_completion();
                progress.page_programmed(page.size, t.elapsed().as_millis());
                t = std::time::Instant::now();
                if let Ok(0) = result {
                } else {
                    return Err(FlashBuilderError::ProgramPage(page.address, 0));
                }

                // Start the next copy process.
                active.start_program_page_with_buffer(current_buf, page.address)?;

                // Swap the buffers
                if current_buf == 1 {
                    current_buf = 0;
                } else {
                    current_buf = 1;
                }
            }

            Ok(())
        });
        progress.finished_programming();
        result
    }
}
