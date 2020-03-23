use std::fmt::{Debug, Formatter, UpperHex};

use crate::config::{FlashAlgorithm, MemoryRange, PageInfo, SectorInfo};

use super::FlashError;

/// A local helper to print all flash location relevant data in hex.
fn fmt_hex<T: UpperHex>(data: &T, f: &mut Formatter) -> std::fmt::Result {
    write!(f, "0X{:08X}", data)
}

/// The description of a page in flash.
#[derive(Clone)]
pub struct FlashPage {
    address: u32,
    data: Vec<u8>,
}

impl Debug for FlashPage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "FlashPage {{")?;
        writeln!(f, "    address: {:#08X}", self.address())?;
        writeln!(f, "    size: {:#08X}", self.size())?;
        writeln!(f, "    data: {:?}", self.data())?;
        write!(f, "}}")
    }
}

impl FlashPage {
    /// Creates a new empty flash page form a `PageInfo`.
    fn new(page_info: &PageInfo) -> Self {
        Self {
            address: page_info.base_address,
            data: vec![0; page_info.size as usize],
        }
    }

    /// Returns the start address of the page.
    pub fn address(&self) -> u32 {
        self.address
    }

    /// Returns the size of the page in bytes.
    pub fn size(&self) -> u32 {
        self.data.len() as u32
    }

    /// Returns the data slice of the page.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Returns the mut data slice of the page.
    pub(super) fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }
}

/// The description of a sector in flash.
#[derive(Derivative)]
#[derivative(Debug, Clone)]
pub struct FlashSector {
    #[derivative(Debug(format_with = "fmt_hex"))]
    address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    size: u32,
}

impl FlashSector {
    /// Creates a new empty flash sector form a `SectorInfo`.
    fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
        }
    }

    /// Returns the start address of the sector.
    pub fn address(&self) -> u32 {
        self.address
    }

    /// Returns the size of the sector in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }
}

/// A struct to hold all the information about one region
/// in the flash that is erased during flashing and has to be restored to it's original value afterwards.
#[derive(Clone)]
pub struct FlashFill {
    address: u32,
    size: u32,
    page_index: usize,
}

impl Debug for FlashFill {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "FlashFill {{")?;
        writeln!(f, "    address: {:#08X}", self.address())?;
        writeln!(f, "    size: {:#08X}", self.size())?;
        writeln!(f, "    page_index: {:?}", self.page_index)?;
        write!(f, "}}")
    }
}

impl FlashFill {
    /// Creates a new empty flash fill.
    fn new(address: u32, size: u32, page_index: usize) -> Self {
        Self {
            address,
            size,
            page_index,
        }
    }

    /// Returns the start address of the fill.
    pub fn address(&self) -> u32 {
        self.address
    }

    /// Returns the size of the fill in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Returns the corresponding page index of the fill.
    pub fn page_index(&self) -> usize {
        self.page_index
    }
}

/// The built layout of the data in flash.
#[derive(Debug, Clone)]
pub struct FlashLayout {
    sectors: Vec<FlashSector>,
    pages: Vec<FlashPage>,
    fills: Vec<FlashFill>,
}

impl FlashLayout {
    /// Get the sectors of the flash layout.
    pub fn sectors(&self) -> &[FlashSector] {
        &self.sectors
    }

    /// Get the pages of the flash layout.
    pub fn pages(&self) -> &[FlashPage] {
        &self.pages
    }

    /// Get the pages of the flash layout as mut.
    pub(super) fn pages_mut(&mut self) -> &mut [FlashPage] {
        &mut self.pages
    }

    /// Get the fills of the flash layout.
    pub fn fills(&self) -> &[FlashFill] {
        &self.fills
    }
}

/// A block of data that is to be written to flash.
#[derive(Clone, Copy)]
pub(super) struct FlashDataBlock<'a> {
    address: u32,
    data: &'a [u8],
}

impl<'a> FlashDataBlock<'a> {
    /// Create a new `FlashDataBlock`.
    fn new(address: u32, data: &'a [u8]) -> Self {
        Self { address, data }
    }

    /// Get the start address of the block.
    pub(super) fn address(&self) -> u32 {
        self.address
    }

    /// Returns the size of the block in bytes.
    pub(super) fn size(&self) -> u32 {
        self.data.len() as u32
    }
}

/// A helper structure to build a flash layout from a set of data blocks.
#[derive(Default)]
pub(super) struct FlashBuilder<'a> {
    data_blocks: Vec<FlashDataBlock<'a>>,
}

impl<'a> FlashBuilder<'a> {
    /// Creates a new `FlashBuilder` with empty data.
    pub(super) fn new() -> Self {
        Self {
            data_blocks: vec![],
        }
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub(super) fn add_data(&mut self, address: u32, data: &'a [u8]) -> Result<(), FlashError> {
        // Add the operation to the sorted data list.
        match self
            .data_blocks
            .binary_search_by_key(&address, |&v| v.address)
        {
            // If it already is present in the list, return an error.
            Ok(_) => return Err(FlashError::DataOverlap(address)),
            // Add it to the list if it is not present yet.
            Err(position) => {
                // If we have a prior block (prevent u32 underflow), check if it's range intersects
                // the range of the block we are trying to insert. If so, return an error.
                if position > 0 {
                    if let Some(block) = self.data_blocks.get(position - 1) {
                        let range = block.address..block.address + block.data.len() as u32;
                        if range.intersects_range(&(address..address + data.len() as u32)) {
                            return Err(FlashError::DataOverlap(address));
                        }
                    }
                }

                // If we have a block after the one we are trying to insert,
                // check if it's range intersects the range of the block we are trying to insert.
                // If so, return an error.
                // We don't add 1 to the position here, because we have not insert an element yet.
                // So the ones on the right are not shifted yet!
                if let Some(block) = self.data_blocks.get(position) {
                    let range = block.address..block.address + block.data.len() as u32;
                    if range.intersects_range(&(address..address + data.len() as u32)) {
                        return Err(FlashError::DataOverlap(address));
                    }
                }

                // If we made it until here, it is safe to insert the block.
                self.data_blocks
                    .insert(position, FlashDataBlock::new(address, data))
            }
        }

        Ok(())
    }

    /// Layouts an entire flash memory.
    ///
    /// If `restore_unwritten_bytes` is `true`, all bytes of a sector,
    /// that are not to be written during flashing will be read from the flash first
    /// and written again once the sector is erased.
    pub(super) fn _build_sectors_and_pages(
        &self,
        flash_algorithm: &FlashAlgorithm,
        mut fill_page: impl FnMut(&mut FlashPage) -> Result<(), FlashError>,
    ) -> Result<FlashLayout, FlashError> {
        let mut sectors: Vec<FlashSector> = Vec::new();
        let mut pages: Vec<FlashPage> = Vec::new();

        for op in &self.data_blocks {
            let mut pos = 0;

            while pos < op.data.len() {
                // Check if the operation is in another sector.
                let flash_address = op.address + pos as u32;

                log::trace!("Checking sector for address {:#08x}", flash_address);

                if let Some(sector) = sectors.last_mut() {
                    // If the address is not in the sector, add a new sector.
                    if flash_address >= sector.address + sector.size {
                        let sector_info = flash_algorithm.sector_info(flash_address);
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
                    } else if let Some(page) = pages.last_mut() {
                        // If the current page does not contain the address.
                        if flash_address >= page.address + page.size() {
                            // Fill any gap at the end of the current page before switching to a new page.
                            fill_page(page)?;

                            let page_info = flash_algorithm.page_info(flash_address);
                            if let Some(page_info) = page_info {
                                let new_page = FlashPage::new(&page_info);
                                pages.push(new_page);
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
                            let space_left_in_page = page.size() - page.data.len() as u32;
                            let space_left_in_data = op.data.len() - pos;
                            let amount =
                                usize::min(space_left_in_page as usize, space_left_in_data);

                            page.data.extend(&op.data[pos..pos + amount]);
                            log::trace!("Added {} bytes to current page", amount);
                            pos += amount;
                        }
                    } else {
                        // If no page is on the sector yet.
                        let page_info = flash_algorithm.page_info(flash_address);
                        if let Some(page_info) = page_info {
                            let new_page = FlashPage::new(&page_info);
                            pages.push(new_page.clone());
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
                    let sector_info = flash_algorithm.sector_info(flash_address);

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
        if let Some(page) = pages.last_mut() {
            fill_page(page)?;
        }

        log::debug!("Sectors are:");
        for sector in &sectors {
            log::debug!("{:#?}", sector);
        }

        log::debug!("Pages are:");
        for page in &pages {
            log::debug!("{:#?}", page);
        }

        Ok(FlashLayout {
            sectors,
            pages,
            fills: vec![],
        })
    }

    pub(super) fn add_sector<'b>(
        &self,
        flash_algorithm: &FlashAlgorithm,
        address: u32,
        sectors: &'b mut Vec<FlashSector>,
    ) -> Result<&'b mut FlashSector, FlashError> {
        let sector_info = flash_algorithm.sector_info(address);
        if let Some(sector_info) = sector_info {
            let new_sector = FlashSector::new(&sector_info);
            sectors.push(new_sector);
            log::trace!(
                "Added Sector (0x{:08x}..0x{:08x})",
                sector_info.base_address,
                sector_info.base_address + sector_info.size
            );
            // We just added a sector, so this unwrap can never fail!
            Ok(sectors.last_mut().unwrap())
        } else {
            Err(FlashError::InvalidFlashAddress(address))
        }
    }

    pub(super) fn add_page<'b>(
        &self,
        flash_algorithm: &FlashAlgorithm,
        address: u32,
        pages: &'b mut Vec<FlashPage>,
    ) -> Result<&'b mut FlashPage, FlashError> {
        let page_info = flash_algorithm.page_info(address);
        if let Some(page_info) = page_info {
            let new_page = FlashPage::new(&page_info);
            pages.push(new_page);
            log::trace!(
                "Added Page (0x{:08x}..0x{:08x})",
                page_info.base_address,
                page_info.base_address + page_info.size
            );
            // We just added a page, so this unwrap can never fail!
            Ok(pages.last_mut().unwrap())
        } else {
            return Err(FlashError::InvalidFlashAddress(address));
        }
    }

    /// Adds a new fill .
    pub(super) fn add_fill<'b>(
        &self,
        address: u32,
        size: u32,
        fills: &'b mut Vec<FlashFill>,
        page_index: usize,
    ) {
        fills.push(FlashFill::new(address, size, page_index));
    }

    pub(super) fn build_sectors_and_pages(
        &self,
        flash_algorithm: &FlashAlgorithm,
    ) -> Result<FlashLayout, FlashError> {
        let mut sectors: Vec<FlashSector> = Vec::new();
        let mut pages: Vec<FlashPage> = Vec::new();
        let mut fills: Vec<FlashFill> = Vec::new();

        let mut data_iter = self.data_blocks.iter().enumerate().peekable();
        while let Some((n, block)) = data_iter.next() {
            let block_end_address = block.address + block.size() as u32;
            let mut block_offset = 0usize;

            while block_offset < block.data.len() {
                let current_block_address = block.address + block_offset as u32;
                let sector = if let Some(sector) = sectors.last_mut() {
                    // If the address is not in the sector, add a new sector.
                    // We only ever need to check the last sector in the list, as all the blocks to be written
                    // are stored in the `flash_write_data` vector IN ORDER!
                    // This means if we are checking the last sector we already have checked previous ones
                    // in previous steps of the iteration.
                    if current_block_address >= sector.address + sector.size {
                        self.add_sector(flash_algorithm, current_block_address, &mut sectors)?
                    } else {
                        sector
                    }
                } else {
                    self.add_sector(flash_algorithm, current_block_address, &mut sectors)?
                };

                let page = if let Some(page) = pages.last_mut() {
                    // If the address is not in the last page, add a new page.
                    // We only ever need to check the last page in the list, as all the blocks to be written
                    // are stored in the `data_blocks` vector IN ORDER!
                    // This means if we are checking the last page we already have checked previous ones
                    // in previous steps of the iteration.
                    if current_block_address >= page.address + page.size() {
                        self.add_page(flash_algorithm, current_block_address, &mut pages)?
                    } else {
                        page
                    }
                } else {
                    self.add_page(flash_algorithm, current_block_address, &mut pages)?
                };

                // Add sectors for the whole page if the sector size is smaller than the page size!
                let sector_size = sector.size;
                let sector_address = sector.address;
                if sector_size < page.size() {
                    // Add as many sectors as there fit into one page.
                    for i in 0..page.size() / sector_size {
                        // Calculate the address of the sector.
                        let new_sector_address = page.address + i * sector_size;

                        // If the sector address does not match the address of the just added sector,
                        // add a new sector at that addresss.
                        if new_sector_address != sector_address {
                            self.add_sector(flash_algorithm, new_sector_address, &mut sectors)?;
                        }
                    }
                }

                let end_address = block_end_address.min(page.address + page.size()) as usize;
                let page_offset = (block.address + block_offset as u32 - page.address) as usize;
                let size = end_address - page_offset - page.address as usize;
                let page_size = page.size();
                let page_address = page.address;

                // Insert the actual data into the page!
                page.data[page_offset..page_offset + size]
                    .copy_from_slice(&block.data[block_offset..block_offset + size]);

                // If we start working a new block (condition: block_offset == 0)
                // and we don't start a new page (condition: page_offset == 0)
                // We need to fill the start of the page up until the page offset where the new data will start.
                if block_offset == 0 && page_offset != 0 {
                    self.add_fill(page_address, page_offset as u32, &mut fills, pages.len());
                }

                // If we have finished writing our block (condition: block_offset + size == block_size)
                // and we have not finished the page yet (condition: page_offset + size == page_size)
                // we peek to the next block and see where it starts and fill the page
                // up to a maximum of the next block start.
                if block_offset + size == block.size() as usize
                    && page_offset + size != page_size as usize
                {
                    // Where the fillup ends which is by default the end of the page.
                    let mut fill_end_address = (page_address + page_size) as usize;

                    // Try to get the address of the next block and adjust the address to it's start
                    // if it is smaller than the end of the last page.
                    if let Some((_, next_block)) = data_iter.peek() {
                        fill_end_address = fill_end_address.min(next_block.address as usize);
                    }

                    // Calculate the start of the fill relative to the page.
                    let fill_start = page_offset + size;
                    // Calculate the fill size.
                    let fill_size = fill_end_address - (page_address as usize + fill_start);

                    // Actually fill the page and register a fill block within the stat tracker.
                    self.add_fill(
                        page_address + fill_start as u32,
                        fill_size as u32,
                        &mut fills,
                        pages.len() - 1,
                    );
                }

                // Denotes whether a new sector will be done next iteration round.
                let start_new_sector =
                    current_block_address + size as u32 >= sector_address + sector_size;
                // Denotes whether we are done with the flash building process now.
                let last_bit_of_block = block_offset + size == block.size() as usize
                    && self.data_blocks.len() > 0
                    && n == self.data_blocks.len() - 1;

                // If one of the two conditions resolves to true, we fill all remaining pages for the current sector.
                if start_new_sector || last_bit_of_block {
                    // Iterate all possible sector pages and see if they have been created yet.
                    let pages_per_sector =
                        (sector_size / flash_algorithm.flash_properties.page_size) as usize;
                    'o: for i in 0..pages_per_sector {
                        // Calculate the possible page address.
                        let page_address =
                            sector_address + i as u32 * flash_algorithm.flash_properties.page_size;
                        // Get the maximum available already added pages up to a maximum of
                        // the available pages per sector.
                        let last_pages_num_max = pages_per_sector.min(pages.len());
                        // Get those pages from the pages vector.
                        let max_last_pages = &pages[pages.len() - last_pages_num_max..];
                        for page in max_last_pages {
                            if page.address == page_address {
                                continue 'o;
                            }
                        }
                        let page = self.add_page(flash_algorithm, page_address, &mut pages)?;
                        self.add_fill(page.address, page.size(), &mut fills, pages.len());
                    }
                }

                // Make sure we advance the block offset by the amount we just wrote.
                block_offset += size;
            }
        }

        // Return the finished flash layout.
        Ok(FlashLayout {
            sectors,
            pages,
            fills,
        })
    }
}

#[cfg(test)]
mod tests {
    use insta::*;

    use super::{FlashBuilder, FlashError, FlashPage};
    use crate::config::{FlashAlgorithm, FlashProperties, SectorDescription};

    fn assemble_demo_flash1() -> FlashAlgorithm {
        let sd = SectorDescription {
            size: 4096,
            address: 0,
        };

        let mut flash_algorithm = FlashAlgorithm::default();
        flash_algorithm.flash_properties = FlashProperties {
            address_range: 0..1 << 16,
            page_size: 1024,
            erased_byte_value: 255,
            program_page_timeout: 200,
            erase_sector_timeout: 200,
            sectors: vec![sd],
        };

        flash_algorithm
    }

    fn assemble_demo_flash2() -> FlashAlgorithm {
        let sd = SectorDescription {
            size: 128,
            address: 0,
        };

        let mut flash_algorithm = FlashAlgorithm::default();
        flash_algorithm.flash_properties = FlashProperties {
            address_range: 0..1 << 16,
            page_size: 1024,
            erased_byte_value: 255,
            program_page_timeout: 200,
            erase_sector_timeout: 200,
            sectors: vec![sd],
        };

        flash_algorithm
    }

    fn fill_page_uniform(
        flash_page: &mut FlashPage,
        range: std::ops::Range<usize>,
    ) -> Result<(), FlashError> {
        for i in range {
            flash_page.data[i] = 123;
        }
        Ok(())
    }

    fn fill_page_increment(
        flash_page: &mut FlashPage,
        range: std::ops::Range<usize>,
    ) -> Result<(), FlashError> {
        for i in range {
            flash_page.data[i] = i as u8;
        }
        Ok(())
    }

    #[test]
    fn add_overlapping_data() {
        let mut flash_builder = FlashBuilder::new();
        assert!(flash_builder.add_data(0, &[42]).is_ok());
        assert!(flash_builder.add_data(0, &[42]).is_err());
    }

    #[test]
    fn add_non_overlapping_data() {
        let mut flash_builder = FlashBuilder::new();
        assert!(flash_builder.add_data(0, &[42]).is_ok());
        assert!(flash_builder.add_data(1, &[42]).is_ok());
    }

    #[test]
    fn single_byte_in_single_page() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_full_single_page() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_one_full_page_one_page_one_byte() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_one_page_from_offset_span_two_pages() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(42, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_four_and_a_half_pages_two_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_in_two_data_chunks_multiple_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_in_two_data_chunks_multiple_sectors_smaller_than_page() {
        let flash_algorithm = assemble_demo_flash2();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm)
            .unwrap();
        assert_debug_snapshot!(flash_layout);
    }
}
