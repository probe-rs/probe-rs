use crate::config::memory::{SectorInfo, PageInfo};
use super::flasher::{Flasher, FlasherError};
use std::mem::swap;

const DATA_TRANSFER_B_PER_S: f32 = 40.0 * 1000.0; // ~40KB/s, depends on clock speed, theoretical limit for HID is 56,000 B/s

#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub struct FlashPage {
    #[derivative(Debug(format_with = "fmt_hex"))]
    address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    size: u32,
    #[derivative(Debug(format_with = "fmt"))]
    data: Vec<u8>,
    pub erased: Option<bool>,
    pub dirty: Option<bool>,
    cached_estimate_data: Vec<u8>,
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
            erased: None,
            dirty: None,
            cached_estimate_data: vec![],
        }
    }

    /// Get time to verify a page.
    pub fn get_verify_weight(&self) -> f32 {
        self.size as f32 / DATA_TRANSFER_B_PER_S
    }
}

#[derive(Derivative)]
#[derivative(Debug, Clone)]
pub struct FlashSector {
    #[derivative(Debug(format_with = "fmt_hex"))]
    address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    size: u32,
    max_page_count: usize,
    pages: Vec<FlashPage>,
}

impl FlashSector {
    pub fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
            max_page_count: 0,
            pages: vec![],
        }
    }

    pub fn add_page(&mut self, page: FlashPage) -> Result<(), FlashBuilderError> {
        if self.size % page.size != 0 {
            return Err(FlashBuilderError::FlashSectorNotMultipleOfPageSize(
                page.size, self.size,
            ));
        }
        let max_page_count = (self.size / page.size) as usize;

        if self.pages.len() < max_page_count {
            self.max_page_count = max_page_count;
            self.pages.push(page);
            self.pages.sort_by_key(|p| p.address);
        } else {
            return Err(FlashBuilderError::MaxPageCountExceeded(max_page_count));
        }
        Ok(())
    }

    pub fn is_pages_to_be_programmed(&self) -> bool {
        self.pages.iter().any(|p| {
            if let Some(true) = p.dirty {
                true
            } else {
                false
            }
        })
    }

    pub fn set_all_pages_dirty(&mut self) {
        for page in &mut self.pages {
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
        Self { address, data }
    }
}

pub struct FlashBuilder<'a> {
    pub(crate) flash_start: u32,
    flash_operations: Vec<FlashOperation<'a>>,
    buffered_data_size: usize,
    enable_double_buffering: bool,
}

#[derive(Debug)]
pub enum FlashBuilderError {
    AddressBeforeFlashStart(u32),               // Contains faulty address.
    DataOverlap(u32),                           // Contains faulty address.
    InvalidFlashAddress(u32),                   // Contains faulty address.
    DoubleDataEntry(u32), // There is two entries for data at the same address.
    FlashSectorNotMultipleOfPageSize(u32, u32), // The flash sector size is not a multiple of the flash page size.
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
    // TODO: Needed when we do advanced flash analysis.
    // // Type of flash analysis
    // FLASH_ANALYSIS_CRC32 = "CRC32"
    // FLASH_ANALYSIS_PARTIAL_PAGE_READ = "PAGE_READ"

    pub fn new(flash_start: u32) -> Self {
        Self {
            flash_start,
            flash_operations: vec![],
            buffered_data_size: 0,
            enable_double_buffering: false,
        }
    }

    pub fn pages(sectors: &[FlashSector]) -> Vec<&FlashPage> {
        sectors.iter().map(|s| &s.pages).flatten().collect()
    }

    pub fn pages_mut(sectors: &mut Vec<FlashSector>) -> Vec<&mut FlashPage> {
        sectors.iter_mut().map(|s| &mut s.pages).flatten().collect()
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub fn add_data(&mut self, address: u32, data: &'a [u8]) -> Result<(), FlashBuilderError> {
        // Add the operation to the sorted data list.
        match self
            .flash_operations
            .binary_search_by_key(&address, |&v| v.address)
        {
            Ok(_) => return Err(FlashBuilderError::DoubleDataEntry(address)),
            Err(position) => self
                .flash_operations
                .insert(position, FlashOperation::new(address, data)),
        }
        self.buffered_data_size += data.len();

        // Verify that the data list does not have overlapping addresses.
        let mut previous_operation: Option<&FlashOperation> = None;
        for operation in &self.flash_operations {
            if let Some(previous) = previous_operation {
                if previous.address + previous.data.len() as u32 > operation.address {
                    return Err(FlashBuilderError::DataOverlap(operation.address));
                }
            }
            previous_operation = Some(operation);
        }
        Ok(())
    }

    fn mark_all_pages_for_programming(sectors: &mut Vec<FlashSector>) {
        for sector in sectors {
            sector.set_all_pages_dirty();
        }
    }

    /// Determine fastest method of flashing and then run flash programming.
    ///
    /// Data must have already been added with add_data
    /// TODO: Not sure if this works as intended ...
    pub fn program(
        &self,
        mut flash: Flasher,
        mut chip_erase: Option<bool>,
        smart_flash: bool,
        keep_unwritten: bool,
    ) -> Result<(), FlashBuilderError> {
        if self.flash_operations.is_empty() {
            // Nothing to do.
            return Ok(());
        }

        let mut sectors = vec![];

        // Convert the list of flash operations into flash sectors and pages.
        self.build_sectors_and_pages(&mut flash, &mut sectors, keep_unwritten)?;
        if sectors.is_empty() || sectors[0].pages.is_empty() {
            // Nothing to do.
            return Ok(());
        }

        log::debug!("Smart Flash enabled: {:?}", smart_flash);
        // If smart flash was set to false then mark all pages as requiring programming.
        if !smart_flash {
            Self::mark_all_pages_for_programming(&mut sectors);
        }

        // If the flash algo doesn't support erase all, disable chip erase.
        if flash.flash_algorithm().pc_erase_all.is_none() {
            chip_erase = Some(false);
        }

        log::debug!("Full Chip Erase enabled: {:?}", chip_erase);
        log::debug!(
            "Double Buffering enabled: {:?}",
            self.enable_double_buffering
        );
        if Some(true) == chip_erase {
            if flash.double_buffering_supported() && self.enable_double_buffering {
                self.chip_erase_program_double_buffer(&mut flash, &sectors)?;
            } else {
                self.chip_erase_program(&mut flash, &sectors)?;
            };
        } else if flash.double_buffering_supported() && self.enable_double_buffering {
            self.sector_erase_program_double_buffer(&mut flash, &mut sectors)?;
        } else {
            // WORKING: We debug this atm.
            self.sector_erase_program(&mut flash, &sectors)?;
        }

        Ok(())
    }

    fn build_sectors_and_pages(
        &self,
        flash: &mut Flasher,
        sectors: &mut Vec<FlashSector>,
        keep_unwritten: bool,
    ) -> Result<(), FlashBuilderError> {
        for op in &self.flash_operations {
            let mut pos = 0;
            while pos < op.data.len() {
                // Check if the operation is in another sector.
                let flash_address = op.address + pos as u32;
                if let Some(sector) = sectors.last_mut() {
                    // If the address is not in the sector, add a new sector.
                    if flash_address >= sector.address + sector.size {
                        let sector_info = flash.region().sector_info(flash_address);
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
                            Self::fill_end_of_page_gap(
                                flash,
                                page,
                                page.size as usize - page.data.len(),
                                keep_unwritten,
                            )?;

                            let page_info = flash.region().page_info(flash_address);
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
                        let page_info = flash.region().page_info(flash_address);
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
                    let sector_info = flash.region().sector_info(flash_address);
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
                }
            }
        }

        // Fill the page gap if there is one.
        if let Some(sector) = sectors.last_mut() {
            if let Some(page) = sector.pages.last_mut() {
                Self::fill_end_of_page_gap(
                    flash,
                    page,
                    page.size as usize - page.data.len(),
                    keep_unwritten,
                )?;
            }
        }

        if keep_unwritten {
            Self::fill_unwritten_sector_pages(flash, sectors)?;
        }

        log::debug!("Sectors are:");
        for sector in sectors {
            log::debug!("{:#?}", sector);
        }

        Ok(())
    }

    fn fill_end_of_page_gap(
        flash: &mut Flasher,
        current_page: &mut FlashPage,
        old_data_len: usize,
        keep_unwritten: bool,
    ) -> Result<(), FlashBuilderError> {
        if current_page.data.len() != current_page.size as usize {
            let page_data_end = current_page.address + current_page.data.len() as u32;

            let old_data = if keep_unwritten {
                let mut data = vec![0; old_data_len];
                flash
                    .run_verify(|active| active.read_block8(page_data_end, data.as_mut_slice()))?;
                data
            } else {
                vec![flash.region().erased_byte_value; old_data_len]
            };
            current_page.data.extend(old_data);
        }
        Ok(())
    }

    fn fill_unwritten_sector_pages(
        flash: &mut Flasher,
        sectors: &mut Vec<FlashSector>,
    ) -> Result<(), FlashBuilderError> {
        for sector_id in 0..sectors.len() {
            let sector_address = sectors[sector_id].address;
            let num_pages = sectors[sector_id].pages.len();
            let mut sector_page_address = sector_address;

            for sector_page_number in 0..num_pages {
                let mut page = &mut sectors[sector_id].pages[sector_page_number];

                if page.address != sector_page_address {
                    page = Self::add_page_with_existing_data(
                        flash,
                        sectors,
                        sector_id,
                        sector_page_address,
                    )?;
                }

                sector_page_address += page.size;
            }
        }
        Ok(())
    }

    fn add_page_with_existing_data<'b>(
        flash: &mut Flasher,
        sectors: &'b mut Vec<FlashSector>,
        sector_id: usize,
        sector_page_address: u32,
    ) -> Result<&'b mut FlashPage, FlashBuilderError> {
        let sector = &mut sectors[sector_id];
        let page_info = flash.region().page_info(sector_page_address);
        let page_info = if let Some(page_info) = page_info {
            page_info
        } else {
            return Err(FlashBuilderError::InvalidFlashAddress(sector_page_address));
        };
        let mut new_page = FlashPage::new(&page_info);
        new_page.data = vec![0; new_page.size as usize];
        new_page.dirty = Some(false);
        flash.run_verify(|active| {
            active.read_block8(new_page.address, new_page.data.as_mut_slice())
        })?;
        sector.add_page(new_page)?;

        let last = sector.pages.len() - 1;
        Ok(&mut sector.pages[last])
    }

    /// Program by first performing a chip erase.
    fn chip_erase_program(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
    ) -> Result<(), FlashBuilderError> {
        flash.run_erase(|active| active.erase_all())?;

        let r: R = flash.run_program(|active| {
            for page in Self::pages(sectors) {
                // TODO: Check this condition.
                if let Some(true) = page.erased {
                    continue;
                } else {
                    active.program_page(page.address, page.data.as_slice())?;
                }
            }
            Ok(())
        });

        r
    }

    fn next_unerased_page(sectors: &[FlashSector], page: u32) -> (Option<&FlashPage>, u32) {
        let pages = Self::pages(sectors);
        for n in page as usize + 1..pages.len() {
            if let Some(page) = pages.get(n) {
                if let Some(true) = page.erased {
                    return (Some(page), n as u32);
                }
            }
        }
        (None, page)
    }

    fn chip_erase_program_double_buffer(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
    ) -> Result<(), FlashBuilderError> {
        flash.run_erase(|active| active.erase_all())?;

        let mut current_buf = 0;
        let mut next_buf = 1;
        let (first_page, i) = Self::next_unerased_page(sectors, 0);

        if let Some(page) = first_page {
            flash.run_program(|active| {
                active.load_page_buffer(page.address, page.data.as_slice(), current_buf)?;

                let mut current_page = first_page;
                let mut i = i;

                while let Some(page) = current_page {
                    active.start_program_page_with_buffer(current_buf, page.address)?;

                    let r = Self::next_unerased_page(sectors, i);
                    current_page = r.0;
                    i = r.1;

                    if let Some(page) = current_page {
                        active.load_page_buffer(page.address, page.data.as_slice(), next_buf)?;
                    }

                    let result = active.wait_for_completion();
                    if let Ok(0) = result {
                    } else {
                        // TODO: Fix me.
                        // return Err(FlashBuilderError::ProgramPage(page.address, result));
                    }

                    swap(&mut current_buf, &mut next_buf);
                }

                Ok(())
            })
        } else {
            Ok(())
        }
    }

    /// Program by performing sector erases.
    fn sector_erase_program(
        &self,
        flash: &mut Flasher,
        sectors: &[FlashSector],
    ) -> Result<(), FlashBuilderError> {
        let number_of_sectors_to_be_programmed = sectors
            .iter()
            .filter(|s| s.is_pages_to_be_programmed())
            .count();
        log::debug!("Flashing {} sectors.", number_of_sectors_to_be_programmed);
        let mut i = 0;
        for sector in sectors {
            if sector.is_pages_to_be_programmed() {
                log::debug!("Erasing sector {}", i);
                flash.run_erase(|active| active.erase_sector(sector.address))?;

                log::debug!("Programming sector {}", i);
                for page in &sector.pages {
                    flash.run_program(|active| {
                        active.program_page(page.address, page.data.as_slice())
                    })?;
                }
            }
            i += 1;
        }
        Ok(())
    }

    fn next_nonsame_page<'b>(pages: &[&'b FlashPage], page: u32) -> (Option<&'b FlashPage>, u32) {
        for n in page as usize + 1..pages.len() {
            if let Some(page) = pages.get(n) {
                if let Some(true) = page.dirty {
                    return (Some(page), n as u32);
                }
            }
        }
        (None, page)
    }

    // TODO: Analyze for sanity. Code looks stupid.
    fn sector_erase_program_double_buffer(
        &self,
        flash: &mut Flasher,
        sectors: &mut Vec<FlashSector>,
    ) -> Result<(), FlashBuilderError> {
        let r: R = flash.run_erase(|active| {
            for sector in sectors.iter_mut() {
                if sector.is_pages_to_be_programmed() {
                    active.erase_sector(sector.address)?;
                }
            }
            Ok(())
        });
        r?;

        let mut current_buf = 0;
        let mut next_buf = 1;
        let (first_page, i) = Self::next_nonsame_page(&Self::pages(sectors), 0);

        if let Some(page) = first_page {
            let r: R = flash.run_program(|active| {
                active.load_page_buffer(page.address, page.data.as_slice(), current_buf)?;

                let mut current_page = first_page;
                let mut i = i;

                while let Some(page) = current_page {
                    if page.dirty.is_some() {
                        active.start_program_page_with_buffer(current_buf, page.address)?;

                        let r = Self::next_nonsame_page(&Self::pages(sectors), i);
                        current_page = r.0;
                        i = r.1;

                        if let Some(page) = current_page {
                            active.load_page_buffer(
                                page.address,
                                page.data.as_slice(),
                                next_buf,
                            )?;
                        }

                        let result = active.wait_for_completion();
                        if let Ok(0) = result {
                        } else {
                            // TODO: Fix me.
                            // return Err(FlashBuilderError::ProgramPage(page.address, result));
                        }

                        swap(&mut current_buf, &mut next_buf);
                    }
                }

                Ok(())
            });
            r?
        }

        Ok(())
    }
}
