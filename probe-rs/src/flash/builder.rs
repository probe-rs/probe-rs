use super::flasher::Flasher;
use crate::config::{PageInfo, SectorInfo};

use super::FlashError;

/// A struct to hold all the information about one page of flash.
#[derive(Derivative, Clone)]
#[derivative(Debug)]
pub(super) struct FlashPage {
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) size: u32,
    #[derivative(Debug(format_with = "fmt"))]
    pub(super) data: Vec<u8>,
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
    pub(super) fn new(page_info: &PageInfo) -> Self {
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
pub(super) struct FlashSector {
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) size: u32,
    pub(super) page_size: u32,
    pub(super) pages: Vec<FlashPage>,
}

impl FlashSector {
    /// Creates a new empty flash sector form a `SectorInfo`.
    pub(super) fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
            page_size: sector_info.page_size,
            pages: vec![],
        }
    }

    /// Adds a new `FlashPage` to the `FlashSector`.
    pub(super) fn add_page(&mut self, page: FlashPage) -> Result<(), FlashError> {
        // If the pages do not align nicely within the sector, return an error.
        if self.page_size != page.size {
            return Err(FlashError::PageSizeDoesNotMatch {
                page_size: page.size,
                sector_page_size: self.page_size,
            });
        }

        // Determine the maximal amout of pages in the sector.
        let max_page_count = (self.size / page.size) as usize;

        // Make sure we haven't reached the sectors maximum capacity yet.
        if self.pages.len() < max_page_count {
            // Add a page and keep the pages sorted.
            self.pages.push(page);
            self.pages.sort_by_key(|p| p.address);
        } else {
            return Err(FlashError::MaxPageCountExceeded {
                maximum_page_count: max_page_count,
                sector_address: self.address,
            });
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct FlashWriteData<'a> {
    pub(super) address: u32,
    pub(super) data: &'a [u8],
}

impl<'a> FlashWriteData<'a> {
    pub(super) fn new(address: u32, data: &'a [u8]) -> Self {
        Self { address, data }
    }
}

#[derive(Default)]
pub(super) struct FlashBuilder<'a> {
    flash_write_data: Vec<FlashWriteData<'a>>,
    buffered_data_size: usize,
}

impl<'a> FlashBuilder<'a> {
    /// Creates a new `FlashBuilder` with empty data.
    pub(super) fn new() -> Self {
        Self {
            flash_write_data: vec![],
            buffered_data_size: 0,
        }
    }

    /// Iterate over all pages in an array of `FlashSector`s.
    pub(super) fn pages(sectors: &[FlashSector]) -> Vec<&FlashPage> {
        sectors.iter().map(|s| &s.pages).flatten().collect()
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub(super) fn add_data(&mut self, address: u32, data: &'a [u8]) -> Result<(), FlashError> {
        // Add the operation to the sorted data list.
        match self
            .flash_write_data
            .binary_search_by_key(&address, |&v| v.address)
        {
            // If it already is present in the list, return an error.
            Ok(_) => return Err(FlashError::DuplicateDataEntry(address)),
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
                    return Err(FlashError::DataOverlap(operation.address));
                }
            }
            previous_operation = Some(operation);
        }
        Ok(())
    }

    /// Layouts an entire flash memory.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    pub(super) fn build_sectors_and_pages(
        &self,
        flash: &mut Flasher,
        restore_unwritten_bytes: bool,
    ) -> Result<Vec<FlashSector>, FlashError> {
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
                            return Err(FlashError::InvalidFlashAddress(flash_address));
                        }
                        continue;
                    } else if let Some(page) = sector.pages.last_mut() {
                        // If the current page does not contain the address.
                        if flash_address >= page.address + page.size {
                            // Fill any gap at the end of the current page before switching to a new page.
                            flash.fill_page(page, restore_unwritten_bytes)?;

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
                                return Err(FlashError::InvalidFlashAddress(flash_address));
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
                            return Err(FlashError::InvalidFlashAddress(flash_address));
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
                        return Err(FlashError::InvalidFlashAddress(flash_address));
                    }
                    continue;
                }
            }
        }

        // Fill the page gap if there is one.
        if let Some(sector) = sectors.last_mut() {
            if let Some(page) = sector.pages.last_mut() {
                flash.fill_page(page, restore_unwritten_bytes)?;
            }
        }

        log::debug!("Sectors are:");
        for sector in &sectors {
            log::debug!("{:#?}", sector);
        }

        Ok(sectors)
    }
}
