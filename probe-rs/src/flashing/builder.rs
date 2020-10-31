use std::fmt::{Debug, Formatter};

use super::{FlashError, FlashVisualizer};
use crate::config::{FlashAlgorithm, MemoryRange, PageInfo, SectorInfo};

/// The description of a page in flash.
#[derive(Clone, PartialEq, Eq)]
pub struct FlashPage {
    address: u32,
    data: Vec<u8>,
}

impl Debug for FlashPage {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlashPage")
            .field("address", &self.address())
            .field("size", &self.size())
            .finish()
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
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlashSector {
    address: u32,
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
/// in the flash that is erased during flashing and has to be restored to its original value afterwards.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlashFill {
    address: u32,
    size: u32,
    page_index: usize,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashLayout {
    sectors: Vec<FlashSector>,
    pages: Vec<FlashPage>,
    fills: Vec<FlashFill>,
    data_blocks: Vec<FlashDataBlockSpan>,
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

    /// Get the datablocks of the flash layout.
    pub fn data_blocks(&self) -> &[FlashDataBlockSpan] {
        &self.data_blocks
    }

    pub fn visualize(&self) -> FlashVisualizer {
        FlashVisualizer::new(&self)
    }
}

/// A block of data that is to be written to flash.
#[derive(Clone, Copy)]
pub(super) struct FlashDataBlock<'data> {
    address: u32,
    data: &'data [u8],
}

impl<'data> FlashDataBlock<'data> {
    /// Create a new `FlashDataBlock`.
    fn new(address: u32, data: &'data [u8]) -> Self {
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

/// A block of data that is to be written to flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashDataBlockSpan {
    address: u32,
    size: u32,
}

impl FlashDataBlockSpan {
    /// Get the start address of the block.
    pub fn address(&self) -> u32 {
        self.address
    }

    /// Returns the size of the block in bytes.
    pub fn size(&self) -> u32 {
        self.size
    }
}

impl<'data> From<FlashDataBlock<'data>> for FlashDataBlockSpan {
    fn from(block: FlashDataBlock) -> Self {
        Self {
            address: block.address(),
            size: block.size(),
        }
    }
}

impl<'data> From<&FlashDataBlock<'data>> for FlashDataBlockSpan {
    fn from(block: &FlashDataBlock) -> Self {
        Self {
            address: block.address(),
            size: block.size(),
        }
    }
}

/// A helper structure to build a flash layout from a set of data blocks.
#[derive(Default)]
pub(super) struct FlashBuilder<'data> {
    data_blocks: Vec<FlashDataBlock<'data>>,
}

impl<'data> FlashBuilder<'data> {
    /// Creates a new `FlashBuilder` with empty data.
    pub(super) fn new() -> Self {
        Self {
            data_blocks: vec![],
        }
    }

    /// Add a block of data to be programmed.
    ///
    /// Programming does not start until the `program` method is called.
    pub(super) fn add_data(&mut self, address: u32, data: &'data [u8]) -> Result<(), FlashError> {
        // Add the operation to the sorted data list.
        match self
            .data_blocks
            .binary_search_by_key(&address, |&v| v.address)
        {
            // If it already is present in the list, return an error.
            Ok(_) => return Err(FlashError::DataOverlap(address)),
            // Add it to the list if it is not present yet.
            Err(position) => {
                // If we have a prior block (prevent u32 underflow), check if its range intersects
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
                // check if its range intersects the range of the block we are trying to insert.
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

    /// Layouts the contents of a flash memory according to the contents of the flash builder.
    pub(super) fn build_sectors_and_pages(
        &self,
        flash_algorithm: &FlashAlgorithm,
        include_empty_pages: bool,
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
                        add_sector(flash_algorithm, current_block_address, &mut sectors)?
                    } else {
                        sector
                    }
                } else {
                    add_sector(flash_algorithm, current_block_address, &mut sectors)?
                };

                let page = if let Some(page) = pages.last_mut() {
                    // If the address is not in the last page, add a new page.
                    // We only ever need to check the last page in the list, as all the blocks to be written
                    // are stored in the `data_blocks` vector IN ORDER!
                    // This means if we are checking the last page we already have checked previous ones
                    // in previous steps of the iteration.
                    if current_block_address >= page.address + page.size() {
                        add_page(flash_algorithm, current_block_address, &mut pages)?
                    } else {
                        page
                    }
                } else {
                    add_page(flash_algorithm, current_block_address, &mut pages)?
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
                            add_sector(flash_algorithm, new_sector_address, &mut sectors)?;
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
                    add_fill(
                        page_address,
                        page_offset as u32,
                        &mut fills,
                        pages.len() - 1,
                    );
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

                    // Try to get the address of the next block and adjust the address to its start
                    // if it is smaller than the end of the last page.
                    if let Some((_, next_block)) = data_iter.peek() {
                        fill_end_address = fill_end_address.min(next_block.address as usize);
                    }

                    // Calculate the start of the fill relative to the page.
                    let fill_start = page_offset + size;
                    // Calculate the fill size.
                    let fill_size = fill_end_address - (page_address as usize + fill_start);

                    // Actually fill the page and register a fill block within the stat tracker.
                    add_fill(
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
                    && !self.data_blocks.is_empty()
                    && n == self.data_blocks.len() - 1;

                // If one of the two conditions resolves to true, and we are including
                // pages which will only contain fill, then we fill all remaining pages
                // for the current sector.
                if (start_new_sector || last_bit_of_block) && include_empty_pages {
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
                        let page = add_page(flash_algorithm, page_address, &mut pages)?;
                        add_fill(page.address, page.size(), &mut fills, pages.len() - 1);
                    }
                }

                // Make sure we advance the block offset by the amount we just wrote.
                block_offset += size;
            }
        }

        // This sort might be avoided if sectors are inserted in the correct order, but since performance
        // is not an issue, this is the easiest way.
        sectors.sort_by_key(|i| i.address);

        // Return the finished flash layout.
        Ok(FlashLayout {
            sectors,
            pages,
            fills,
            data_blocks: self.data_blocks.iter().map(Into::into).collect(),
        })
    }
}

/// Adds a new sector to the sectors.
fn add_sector<'sector>(
    flash_algorithm: &FlashAlgorithm,
    address: u32,
    sectors: &'sector mut Vec<FlashSector>,
) -> Result<&'sector mut FlashSector, FlashError> {
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

/// Adds a new page to the pages.
fn add_page<'page>(
    flash_algorithm: &FlashAlgorithm,
    address: u32,
    pages: &'page mut Vec<FlashPage>,
) -> Result<&'page mut FlashPage, FlashError> {
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
        Err(FlashError::InvalidFlashAddress(address))
    }
}

/// Adds a new fill to the fills.
fn add_fill(address: u32, size: u32, fills: &mut Vec<FlashFill>, page_index: usize) {
    fills.push(FlashFill::new(address, size, page_index));
}

#[cfg(test)]
mod tests {
    use super::*;
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
            sectors: std::borrow::Cow::Owned(vec![sd]),
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
            sectors: std::borrow::Cow::Owned(vec![sd]),
        };

        flash_algorithm
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
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![FlashSector {
                    address: 0x0000,
                    size: 0x1000,
                },],
                pages: vec![
                    FlashPage {
                        address: 0x0000,
                        data: {
                            let mut data = vec![0; 1024];
                            data[0] = 42;
                            data
                        },
                    },
                    FlashPage {
                        address: 0x0400,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0001,
                        size: 0x03FF,
                        page_index: 0,
                    },
                    FlashFill {
                        address: 0x0400,
                        size: 0x0400,
                        page_index: 1,
                    },
                    FlashFill {
                        address: 0x0800,
                        size: 0x0400,
                        page_index: 2,
                    },
                    FlashFill {
                        address: 0x0C00,
                        size: 0x0400,
                        page_index: 3,
                    }
                ],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 0,
                    size: 1,
                }],
            }
        )
    }

    #[test]
    fn equal_bytes_full_single_page() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![FlashSector {
                    address: 0x0000,
                    size: 0x1000,
                },],
                pages: vec![
                    FlashPage {
                        address: 0x0000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x0400,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0400,
                        size: 0x0400,
                        page_index: 1,
                    },
                    FlashFill {
                        address: 0x0800,
                        size: 0x0400,
                        page_index: 2,
                    },
                    FlashFill {
                        address: 0x0C00,
                        size: 0x0400,
                        page_index: 3,
                    }
                ],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 0,
                    size: 1024,
                }],
            }
        )
    }

    #[test]
    fn equal_bytes_one_full_page_one_page_one_byte() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![FlashSector {
                    address: 0x0000,
                    size: 0x1000,
                },],
                pages: vec![
                    FlashPage {
                        address: 0x0000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x0400,
                        data: {
                            let mut data = vec![0; 1024];
                            data[0] = 42;
                            data
                        },
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0401,
                        size: 0x03FF,
                        page_index: 1,
                    },
                    FlashFill {
                        address: 0x0800,
                        size: 0x0400,
                        page_index: 2,
                    },
                    FlashFill {
                        address: 0x0C00,
                        size: 0x0400,
                        page_index: 3,
                    }
                ],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 0,
                    size: 1025,
                }],
            }
        )
    }

    #[test]
    fn equal_bytes_one_full_page_one_page_one_byte_skip_fill() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, false)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![FlashSector {
                    address: 0x0000,
                    size: 0x1000,
                },],
                pages: vec![
                    FlashPage {
                        address: 0x0000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x0400,
                        data: {
                            let mut data = vec![0; 1024];
                            data[0] = 42;
                            data
                        },
                    },
                ],
                fills: vec![FlashFill {
                    address: 0x0401,
                    size: 0x03FF,
                    page_index: 1,
                },],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 0,
                    size: 1025,
                }],
            }
        )
    }

    #[test]
    fn equal_bytes_one_page_from_offset_span_two_pages() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(42, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![FlashSector {
                    address: 0x000000,
                    size: 0x001000,
                },],
                pages: vec![
                    FlashPage {
                        address: 0x000000,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[..42] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x000400,
                        data: {
                            let mut data = vec![0; 1024];
                            for d in &mut data[..42] {
                                *d = 42;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x000800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x000C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x000000,
                        size: 0x00002A,
                        page_index: 0,
                    },
                    FlashFill {
                        address: 0x00042A,
                        size: 0x0003D6,
                        page_index: 1,
                    },
                    FlashFill {
                        address: 0x000800,
                        size: 0x000400,
                        page_index: 2,
                    },
                    FlashFill {
                        address: 0x000C00,
                        size: 0x000400,
                        page_index: 3,
                    },
                ],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 42,
                    size: 1024,
                },],
            }
        )
    }

    #[test]
    fn equal_bytes_four_and_a_half_pages_two_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![
                    FlashSector {
                        address: 0x000000,
                        size: 0x001000,
                    },
                    FlashSector {
                        address: 0x001000,
                        size: 0x001000,
                    },
                ],
                pages: vec![
                    FlashPage {
                        address: 0x000000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000400,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000800,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000C00,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x001000,
                        data: {
                            let mut data = vec![0; 1024];
                            for d in &mut data[..928] {
                                *d = 42;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001400,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x001800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0013A0,
                        size: 0x000060,
                        page_index: 4,
                    },
                    FlashFill {
                        address: 0x001400,
                        size: 0x000400,
                        page_index: 5,
                    },
                    FlashFill {
                        address: 0x001800,
                        size: 0x000400,
                        page_index: 6,
                    },
                    FlashFill {
                        address: 0x001C00,
                        size: 0x000400,
                        page_index: 7,
                    },
                ],
                data_blocks: vec![FlashDataBlockSpan {
                    address: 0,
                    size: 5024,
                },],
            }
        )
    }

    #[test]
    fn equal_bytes_in_two_data_chunks_multiple_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: vec![
                    FlashSector {
                        address: 0x000000,
                        size: 0x001000,
                    },
                    FlashSector {
                        address: 0x001000,
                        size: 0x001000,
                    },
                    FlashSector {
                        address: 0x002000,
                        size: 0x001000,
                    },
                    FlashSector {
                        address: 0x003000,
                        size: 0x001000,
                    },
                ],
                pages: vec![
                    FlashPage {
                        address: 0x000000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000400,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000800,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000C00,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x001000,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[928..1024] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[..692] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001400,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x001800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x002000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002400,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002800,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002C00,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x003000,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[596..1024] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x003400,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x003800,
                        data: vec![0; 1024],
                    },
                    FlashPage {
                        address: 0x003C00,
                        data: vec![0; 1024],
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0013A0,
                        size: 0x000060,
                        page_index: 4,
                    },
                    FlashFill {
                        address: 0x001C00,
                        size: 0x0002B4,
                        page_index: 5,
                    },
                    FlashFill {
                        address: 0x001400,
                        size: 0x000400,
                        page_index: 6,
                    },
                    FlashFill {
                        address: 0x001800,
                        size: 0x000400,
                        page_index: 7,
                    },
                    FlashFill {
                        address: 0x003254,
                        size: 0x0001AC,
                        page_index: 12,
                    },
                    FlashFill {
                        address: 0x003400,
                        size: 0x000400,
                        page_index: 13,
                    },
                    FlashFill {
                        address: 0x003800,
                        size: 0x000400,
                        page_index: 14,
                    },
                    FlashFill {
                        address: 0x003C00,
                        size: 0x000400,
                        page_index: 15,
                    },
                ],
                data_blocks: vec![
                    FlashDataBlockSpan {
                        address: 0,
                        size: 5024,
                    },
                    FlashDataBlockSpan {
                        address: 7860,
                        size: 5024,
                    },
                ],
            }
        )
    }

    #[test]
    fn two_data_chunks_multiple_sectors_smaller_than_page() {
        let flash_algorithm = assemble_demo_flash2();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, true)
            .unwrap();

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: {
                    let mut sectors = Vec::with_capacity(88);
                    for i in 0..40 {
                        sectors.push(FlashSector {
                            address: 128 * i as u32,
                            size: 0x000080,
                        });
                    }

                    for i in 56..104 {
                        sectors.push(FlashSector {
                            address: 128 * i as u32,
                            size: 0x000080,
                        });
                    }

                    sectors
                },
                pages: vec![
                    FlashPage {
                        address: 0x000000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000400,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000800,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x000C00,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x001000,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[928..1024] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[..692] {
                                *d = 0;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x002000,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002400,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002800,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x002C00,
                        data: vec![42; 1024],
                    },
                    FlashPage {
                        address: 0x003000,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[596..1024] {
                                *d = 0;
                            }
                            data
                        },
                    },
                ],
                fills: vec![
                    FlashFill {
                        address: 0x0013A0,
                        size: 0x000060,
                        page_index: 4,
                    },
                    FlashFill {
                        address: 0x001C00,
                        size: 0x0002B4,
                        page_index: 5,
                    },
                    FlashFill {
                        address: 0x003254,
                        size: 0x0001AC,
                        page_index: 10,
                    }
                ],
                data_blocks: vec![
                    FlashDataBlockSpan {
                        address: 0,
                        size: 5024,
                    },
                    FlashDataBlockSpan {
                        address: 7860,
                        size: 5024,
                    },
                ],
            }
        )
    }
}
