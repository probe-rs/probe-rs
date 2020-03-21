use crate::config::{FlashAlgorithm, MemoryRange, PageInfo, SectorInfo};

use super::FlashError;

/// A struct to hold all the information about one page of the  flash.
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
    write!(f, "{:?}", data)
}

fn fmt_hex<T: std::fmt::LowerHex>(
    data: &T,
    f: &mut std::fmt::Formatter,
) -> Result<(), std::fmt::Error> {
    write!(f, "0x{:08x}", data)
}

impl FlashPage {
    fn new(page_info: &PageInfo) -> Self {
        Self {
            address: page_info.base_address,
            size: page_info.size,
            data: vec![0; page_info.size as usize],
        }
    }
}

/// A struct to hold all the information about one Sector in the flash.
#[derive(Derivative)]
#[derivative(Debug, Clone)]
pub(super) struct FlashSector {
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) address: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) size: u32,
    #[derivative(Debug(format_with = "fmt_hex"))]
    pub(super) page_size: u32,
}

impl FlashSector {
    /// Creates a new empty flash sector form a `SectorInfo`.
    fn new(sector_info: &SectorInfo) -> Self {
        Self {
            address: sector_info.base_address,
            size: sector_info.size,
            page_size: sector_info.page_size,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct FlashLayout {
    sectors: Vec<FlashSector>,
    pages: Vec<FlashPage>,
}

impl FlashLayout {
    pub(super) fn sectors(&self) -> &[FlashSector] {
        &self.sectors
    }

    pub(super) fn pages(&self) -> &[FlashPage] {
        &self.pages
    }
}

#[derive(Clone, Copy)]
pub(super) struct FlashDataBlock<'a> {
    address: u32,
    data: &'a [u8],
}

impl<'a> FlashDataBlock<'a> {
    fn new(address: u32, data: &'a [u8]) -> Self {
        Self { address, data }
    }

    pub(super) fn address(&self) -> u32 {
        self.address
    }

    pub(super) fn size(&self) -> u32 {
        self.data.len() as u32
    }
}

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

    pub(super) fn data_blocks(&self) -> &[FlashDataBlock<'a>] {
        &self.data_blocks
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
                        if flash_address >= page.address + page.size {
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

        Ok(FlashLayout { sectors, pages })
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

    pub(super) fn build_sectors_and_pages(
        &self,
        flash_algorithm: &FlashAlgorithm,
        mut fill_page: impl FnMut(&mut FlashPage, std::ops::Range<usize>) -> Result<(), FlashError>,
    ) -> Result<FlashLayout, FlashError> {
        let mut sectors: Vec<FlashSector> = Vec::new();
        let mut pages: Vec<FlashPage> = Vec::new();
        let mut last_address = 0;

        let mut data_iter = self.data_blocks.iter();
        while let Some(block) = data_iter.next() {
            let block_end_address = block.address + block.data.len() as u32;
            let mut block_offset = 0usize;

            while block_offset < block.data.len() {
                let current_block_address = block.address + block_offset as u32;
                if let Some(sector) = sectors.last_mut() {
                    // If the address is not in the sector, add a new sector.
                    // We only ever need to check the last sector in the list, as all the blocks to be written
                    // are stored in the `flash_write_data` vector IN ORDER!
                    // This means if we are checking the last sector we already have checked previous ones
                    // in previous steps of the iteration.
                    if current_block_address >= sector.address + sector.size {
                        let _ =
                            self.add_sector(flash_algorithm, current_block_address, &mut sectors)?;
                    }
                } else {
                    let _ =
                        self.add_sector(flash_algorithm, current_block_address, &mut sectors)?;
                }

                let page = if let Some(page) = pages.last_mut() {
                    // If the address is not in the last page, add a new page.
                    // We only ever need to check the last page in the list, as all the blocks to be written
                    // are stored in the `data_blocks` vector IN ORDER!
                    // This means if we are checking the last page we already have checked previous ones
                    // in previous steps of the iteration.
                    if current_block_address >= page.address + page.size {
                        self.add_page(flash_algorithm, current_block_address, &mut pages)?
                    } else {
                        page
                    }
                } else {
                    self.add_page(flash_algorithm, current_block_address, &mut pages)?
                };

                let end_address = block_end_address.min(page.address + page.size) as usize;
                let page_offset = (block.address + block_offset as u32 - page.address) as usize;
                let size = end_address - page_offset - page.address as usize;

                // If we write the first block ever and the page_offset is non-zero,
                // we do not write to the start of the page, meaning we have to fill the start of the page.
                // We know that we are writing the first block ever if the `last_address` is still zero.
                if last_address == 0 && page_offset != 0 {
                    fill_page(page, 0..page_offset)?;
                }

                // If the amount
                if page_offset + size != page.size as usize
                    && block_offset + size == block.data.len()
                {
                    fill_page(page, page_offset + size..page.size as usize)?;
                }

                // If the address inside the block we are writing from to the page does not match
                // the last address we wrote to, we have to fill the start of the page until that offset
                // where we are writing at.
                if last_address != block.address + block_offset as u32 {
                    fill_page(page, 0..page_offset)?;
                }

                page.data[page_offset..page_offset + size]
                    .copy_from_slice(&block.data[block_offset..block_offset + size]);

                block_offset += size;

                // Set the last visited address to the end of the currently written block of data.
                last_address = block.address + (block_offset + size) as u32;
            }
        }

        Ok(FlashLayout { sectors, pages })
    }
}

#[cfg(test)]
mod tests {
    use insta::*;

    use super::super::FlashVisualizer;
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
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("single_byte_in_single_page.svg");
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_full_single_page() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("equal_bytes_full_single_page.svg");
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_one_full_page_one_page_one_byte() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("equal_bytes_one_full_page_one_page_one_byte.svg");
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_one_page_from_offset_span_two_pages() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(42, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("equal_bytes_one_page_from_offset_span_two_pages.svg");
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_four_and_a_half_pages_two_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("equal_bytes_four_and_a_half_pages_two_sectors.svg");
        assert_debug_snapshot!(flash_layout);
    }

    #[test]
    fn equal_bytes_in_two_data_chunks_multiple_sectors() {
        let flash_algorithm = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&flash_algorithm, |_, _| Ok(()))
            .unwrap();
        let _ = FlashVisualizer::new(&flash_layout, flash_builder.data_blocks())
            .write_svg("equal_bytes_in_two_data_chunks_multiple_sectors.svg");
        assert_debug_snapshot!(flash_layout);
    }
}
