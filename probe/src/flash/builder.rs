use super::*;
use ::memory::MI;

const PAGE_ESTIMATE_SIZE: u32 = 32;
const PAGE_READ_WEIGHT: f32 = 0.3;
const DATA_TRANSFER_B_PER_S: f32 = 40.0 * 1000.0; // ~40KB/s, depends on clock speed, theoretical limit for HID is 56,000 B/s

pub struct FlashPage {
    address: u32,
    size: u32,
    data: Vec<u8>,
    program_weight: f32,
    pub erased: Option<bool>,
    pub dirty: Option<bool>,
    cached_estimate_data: Vec<u8>,
}

impl FlashPage {
    pub fn new(page_info: &PageInfo) -> Self {
        Self {
            address: page_info.base_address,
            size: page_info.size,
            data: vec![],
            program_weight: page_info.program_weight,
            erased: None,
            dirty: None,
            cached_estimate_data: vec![],
        }
    }

    /// Get time to program a page including the data transfer.
    fn get_program_weight(&self) -> f32 {
        self.program_weight + self.data.len() as f32 / DATA_TRANSFER_B_PER_S
    }

    /// Get time to verify a page.
    pub fn get_verify_weight(&self) -> f32 {
        self.size as f32 / DATA_TRANSFER_B_PER_S
    }
}

pub struct FlashSector {
    address: u32,
    size: u32,
    max_page_count: usize,
    pages: Vec<FlashPage>,
    erase_weight: f32,
}

impl FlashSector {
    pub fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
            max_page_count: 0,
            pages: vec![],
            erase_weight: sector_info.erase_weight,
        }
    }

    pub fn add_page(&mut self, page: FlashPage) -> Result<(), FlashBuilderError> {
        if self.pages.len() == 0 {
            if self.size % page.size != 0 {
                return Err(FlashBuilderError::FlashSectorNotMultipleOfPageSize(page.size, self.size));
            }
            let max_page_count = (self.size / page.size) as usize;

            if self.pages.len() < max_page_count {
                self.max_page_count = max_page_count;
                self.pages.push(page);
                self.pages.sort_by_key(|p| p.address);
            } else {
                return Err(FlashBuilderError::MaxPageCountExceeded(max_page_count));
            }
        }
        Ok(())
    }

    pub fn is_pages_to_be_programmed(&self) -> bool {
        self.pages.iter().any(|p| if let Some(true) = p.dirty { false } else { true })
    }

    pub fn set_all_pages_dirty(&mut self) {
        for page in self.pages {
            page.dirty = Some(true)
        }
    }
}

#[derive(Clone, Copy)]
struct FlashOperation<'a> {
    pub address: u32,
    pub data: &'a [u8],
}

impl<'a> FlashOperation<'a> {
    pub fn new(address: u32, data: &'a [u8]) -> Self {
        Self {
            address,
            data,
        }
    }
}

pub struct FlashBuilder<'a> {
    pub(crate) flash_start: u32,
    flash_operations: Vec<FlashOperation<'a>>,
    buffered_data_size: usize,
    flash: InactiveFlasher<'a>,
    sectors: Vec<FlashSector>,
    enable_double_buffering: bool,
}

pub enum FlashBuilderError {
    AddressBeforeFlashStart(u32), // Contains faulty address.
    DataOverlap(u32), // Contains faulty address.
    InvalidFlashAddress(u32), // Contains faulty address.
    DoubleDataEntry(u32), // There is two entries for data at the same address.
    FlashSectorNotMultipleOfPageSize(u32, u32), // The flash sector size is not a multiple of the flash page size.
    MaxPageCountExceeded(usize),
}

impl<'a> FlashBuilder<'a> {

    // TODO: Needed when we do advanced flash analysis.
    // // Type of flash analysis
    // FLASH_ANALYSIS_CRC32 = "CRC32"
    // FLASH_ANALYSIS_PARTIAL_PAGE_READ = "PAGE_READ"

    pub fn new(flash: InactiveFlasher<'a>) -> Self {
        let flash_start = flash.region().range.start;
        Self {
            flash,
            flash_start: flash_start,
            flash_operations: vec![],
            buffered_data_size: 0,
            sectors: vec![],
            enable_double_buffering: false,
        }
    }
    
    pub fn pages(&self) -> Vec<&FlashPage> {
        self.sectors.iter().map(|s| &s.pages).flatten().collect()
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub fn add_data(&mut self, address: u32, data: &'a [u8]) -> Result<(), FlashBuilderError> {
        // Do a sanity check.
        if self.flash.region().range.contains_range(&(address..address + data.len() as u32)) {
            // Add the operation to the sorted data list.
            match self.flash_operations.binary_search_by_key(&address, |&v| v.address) {
                Ok(_) => { return Err(FlashBuilderError::DoubleDataEntry(address)) },
                Err(position) => self.flash_operations.insert(position, FlashOperation::new(address, data))
            }
            self.buffered_data_size += data.len();

            // Verify that the data list does not have overlapping addresses.
            let mut previous_operation: Option<FlashOperation> = None;
            for operation in self.flash_operations {
                if let Some(previous) = previous_operation {
                    if previous.address + previous.data.len() as u32 > operation.address {
                        return Err(FlashBuilderError::DataOverlap(operation.address));
                    }
                }
                previous_operation = Some(operation);
            }
            Ok(())
        } else {
            Err(FlashBuilderError::AddressBeforeFlashStart(address))
        }
    }

    fn mark_all_pages_for_programming(&mut self) {
        for sector in self.sectors {
            sector.set_all_pages_dirty();
        }
    }

    /// Determine fastest method of flashing and then run flash programming.
    ///
    /// Data must have already been added with add_data
    /// TODO: Not sure if this works as intended ...
    pub fn program(self, chip_erase: Option<bool>, smart_flash: bool) -> Result<(), FlashBuilderError> {        
        // Disable smart options if attempting to read erased sectors will fail.
        let (smart_flash, fast_verify, keep_unwritten) = if !self.flash.region().are_erased_sectors_readable {
            (false, false, false)
        } else {
            (true, true, true)
        };

        if self.flash_operations.len() == 0 {
            // Nothing to do.
            return Ok(())
        }

        // Convert the list of flash operations into flash sectors and pages.
        self.build_sectors_and_pages(keep_unwritten);
        if self.sectors.len() == 0 || self.sectors[0].pages.len() == 0 {
            // Nothing to do.
            return Ok(())
        }

        // If smart flash was set to false then mark all pages as requiring programming.
        if !smart_flash {
            self.mark_all_pages_for_programming();
        }
        
        // If the flash algo doesn't support erase all, disable chip erase.
        if !self.flash.flash_algorithm().pc_erase_all.is_some() {
            chip_erase = Some(false);
        }

        let (chip_erase_count, chip_erase_program_time) = self.compute_chip_erase_pages_and_weight();
        let sector_erase_min_program_time = self.compute_sector_erase_pages_weight_min();

        // If chip_erase hasn't been specified determine if chip erase is faster
        // than page erase regardless of contents
        if chip_erase.is_none() && (chip_erase_program_time < sector_erase_min_program_time) {
            chip_erase = Some(true);
        }

        if Some(true) != chip_erase {
            let (sector_erase_count, page_program_time) = self.compute_sector_erase_pages_and_weight(fast_verify);
            if let None = chip_erase {
                chip_erase = Some(chip_erase_program_time < page_program_time);
            }
        }

        if Some(true) == chip_erase {
            if self.flash.double_buffering_supported() && self.enable_double_buffering {
                self.chip_erase_program_double_buffer()
            } else {
                self.chip_erase_program()
            };
        } else {
            if self.flash.double_buffering_supported() && self.enable_double_buffering {
                self.sector_erase_program_double_buffer()
            } else {
                self.sector_erase_program()
            };
        }

        

        Ok(())
    }

    fn build_sectors_and_pages(&self, keep_unwritten: bool) -> Result<(), FlashBuilderError> {
        let mut program_byte_count = 0;

        let mut flash_address = self.flash_operations[0].address;
        
        // Get sector info and make sure all data is valid.
        let sector_info = self.flash.region().get_sector_info(flash_address);
        let mut sector_info = if let Some(sector_info) = sector_info {
            sector_info
        } else {
            return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
        };

        // Get page info and make sure all data is valid.
        let page_info = self.flash.region().get_page_info(flash_address);
        let mut page_info = if let Some(page_info) = page_info {
            page_info
        } else {
            return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
        };

        let mut current_sector = FlashSector::new(&sector_info);
        let mut current_page = FlashPage::new(&page_info);
        // TODO: This always adds a new sector??? Maybe retrieve the extisting one.
        current_sector.add_page(current_page);
        self.sectors.push(current_sector);
        // TODO: Maybe self.pages.add(current_page);

        for flash_operation in self.flash_operations {
            let mut pos = 0;
            while pos < flash_operation.data.len() {
                // Check if the operation is in another sector.
                flash_address = flash_operation.address + pos as u32;
                if flash_address >= current_sector.address + current_sector.size {
                    let sector_info = self.flash.region().get_sector_info(flash_address);
                    if let Some(sector_info) = sector_info {
                        current_sector = FlashSector::new(&sector_info); 
                        self.sectors.push(current_sector);
                    } else {
                        return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                    }
                }

                // Check if the operation is in another page.
                if flash_address >= current_sector.address + current_sector.size {
                    // Fill any gap at the end of the current page before switching to a new page.
                    self.fill_end_of_page_gap(
                        &current_page,
                        current_page.size as usize - current_page.data.len(),
                        keep_unwritten
                    );

                    let page_info = self.flash.region().get_page_info(flash_address);
                    if let Some(page_info) = page_info {
                        current_page = FlashPage::new(&page_info); 
                        current_sector.add_page(current_page);
                        // TODO: Maybe self.pages.add(current_page);
                    } else {
                        return Err(FlashBuilderError::InvalidFlashAddress(flash_address));
                    }
                }

                // Fill the page gap if there is one.
                self.fill_end_of_page_gap(
                    &current_page,
                    (flash_address - (current_page.address + current_page.data.len() as u32)) as usize,
                    keep_unwritten
                );

                // Copy data to page and increment pos
                let space_left_in_page = page_info.size - current_page.data.len() as u32;
                let space_left_in_data = flash_operation.data.len() - pos;
                let amount = usize::min(space_left_in_page as usize, space_left_in_data);
                current_page.data.extend(&flash_operation.data[pos..pos + amount]);
                program_byte_count += amount;

                // increment position
                pos += amount;
            }
        }

        // Fill the page gap if there is one.
        self.fill_end_of_page_gap(
            &current_page,
            current_page.size as usize - current_page.data.len(),
            keep_unwritten
        );

        if keep_unwritten && self.flash.region().access.contains(Access::R) {
            self.fill_unwritten_sector_pages();
        }

        Ok(())
    }

    fn fill_end_of_page_gap(&self, current_page: &FlashPage, old_data_len: usize, keep_unwritten: bool) {
        if current_page.data.len() != current_page.size as usize {
            let page_data_end = current_page.address + current_page.data.len() as u32;

            let old_data = if keep_unwritten && self.flash.region().access.contains(Access::R) {
                let mut data = vec![0; old_data_len];
                self.flash.run_verify(|active| {
                    active.read_block8(page_data_end, data.as_mut_slice());
                });
                data
            } else {
                vec![self.flash.region().erased_byte_value; old_data_len]
            };
            current_page.data.extend(old_data);
        }
    }

    fn fill_unwritten_sector_pages(&self) -> Result<(), FlashBuilderError> {
        for sector in self.sectors {
            let mut sector_page_address = sector.address;

            for sector_page_number in 0..sector.pages.len() {
                let mut page = sector.pages[sector_page_number];

                if page.address != sector_page_address {
                    page = self.add_page_with_existing_data(&sector, sector_page_address)?;
                }

                sector_page_address += page.size;
            }
        }
        Ok(())
    }

    fn add_page_with_existing_data(&self, sector: &FlashSector, sector_page_address: u32) -> Result<FlashPage, FlashBuilderError> {
        let page_info = self.flash.region().get_page_info(sector_page_address);
        let mut page_info = if let Some(page_info) = page_info {
            page_info
        } else {
            return Err(FlashBuilderError::InvalidFlashAddress(sector_page_address));
        };
        let new_page = FlashPage::new(&page_info);
        new_page.data = vec![0; new_page.size as usize];
        new_page.dirty = Some(false);
        self.flash.run_verify(|active| {
            active.read_block8(new_page.address, new_page.data.as_mut_slice());
        });
        sector.add_page(new_page);
        // TODO: Maybe self.pages.add(current_page);
        Ok(new_page)
    }

    /// Compute the number of erased pages.
    ///
    /// Determine how many pages in the new data are already erased.
    fn compute_chip_erase_pages_and_weight(&self) -> (u32, f32) {
        let mut chip_erase_count: u32 = 0;
        // TODO: Fix the `get_flash_info` param.
        let mut chip_erase_weight: f32 = self.flash.region().get_flash_info(true).erase_weight;
        for page in self.pages() {
            if let Some(erased) = page.erased {
                if !erased {
                    chip_erase_count += 1;
                    chip_erase_weight += page.get_program_weight();
                    // TODO: check if this next line is valid.
                    page.erased = Some(self.flash.region().is_erased(page.data.as_slice()));
                }
            } else {
                page.erased = Some(self.flash.region().is_erased(page.data.as_slice()));
            }
        }
        // TODO: pot. set
        // self.chip_erase_count = chip_erase_count
        // self.chip_erase_weight = chip_erase_weight
        (chip_erase_count, chip_erase_weight)
    }

    fn compute_sector_erase_pages_weight_min(&self) -> f32 {
        self.pages().iter().map(|p| p.get_verify_weight()).sum()
    }

    fn analyze_pages_with_partial_read(&self) {
        for page in self.pages() {
            if let None = page.dirty {
                let size = (PAGE_ESTIMATE_SIZE as usize).min(page.data.len());
                let mut data = vec![0; size];
                self.flash.run_verify(|active| {
                    active.read_block8(page.address, data.as_mut_slice());
                });
                let page_dirty = data != &page.data[0..size];
                if page_dirty {
                    page.dirty = Some(true);
                } else {
                    // Store the read data to avoid further reads.
                    page.cached_estimate_data = data;
                }
            }
        }
    }

    fn analyze_pages_with_crc32(&self, assume_estimate_correct: bool) {
        let mut sectors = vec![];
        let mut pages = vec![];

        // Build a list of all pages to be analyzed.
        for page in self.pages() {
            if let None = page.dirty {
                sectors.push((page.address, page.size));
                pages.push(page);

                let mut data = page.data.clone();
                let pad_size = page.size as usize - page.data.len();
                if pad_size > 0 {
                    data.extend(&vec![0xFFu8, pad_size]);
                }
                page.crc = crc32(data) & 0xFFFF_FFFF;
            }
        }

        // Analyze pages.
        if pages.len() > 0 {
            self.enable_read_access();
            let crcs = self.flash.compute_crcs(sector_list);
            for (page, crc) in pages.zip(crcs) {
                let page_same = page.crc == crc;
                if assume_estimate_correct {
                    page.same = page_same;
                } else if !page_same {
                    page.same = false;
                }
            }
        }
    }

    fn compute_sector_erase_pages_and_weight(&self, fast_verify: bool) -> (u32, f32) {
        unimplemented!();
    }

    /// Program by first performing a chip erase.
    fn chip_erase_program(&mut self) {
        self.flash.run_erase(|active| {
            active.erase_all();
        });
        
        self.flash.run_program(|active| {
            for page in self.pages() {
                if let Some(erased) = page.erased {
                    if !erased {
                        active.program_page(page.address, page.data.as_slice());
                    }
                }
            }
        });
    }

    fn chip_erase_program_double_buffer(&self) {
        unimplemented!();
    }

    /// Program by performing sector erases.
    fn page_erase_program(&self) {
        for page in self.pages() {
            // Read page data if unknown - after this page.same will be True or False
            if let Some(same) = page.same {
                // Program page if not the same
                if !same {
                    self.flash.run_erase(|active| {
                        active.erase_sector(page.address);
                    });

                    self.flash.run_program(|active| {
                        active.program_page(page.address, page.data.as_slice());
                    });
                }
            } else {
                let data = self.flash.target.read_memory_block8(page.address, page.data.len());
                page.same = Some(same(page.data.as_slice(), data));
            }
        }
    }

    fn sector_erase_program_double_buffer(&self) {
        unimplemented!();
    }

    fn sector_erase_program(&self) {
        unimplemented!();
    }
}