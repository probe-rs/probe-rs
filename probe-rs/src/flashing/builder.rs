use std::collections::BTreeMap;
use std::fmt::{Debug, Formatter};
use std::ops::Range;

use probe_rs_target::{MemoryRange, NvmRegion, PageInfo};

use super::{FlashAlgorithm, FlashError};

/// The description of a page in flash.
#[derive(Clone, PartialEq, Eq)]
pub struct FlashPage {
    pub(super) address: u64,
    pub(super) data: Vec<u8>,
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
    /// Creates a new empty flash page from a `PageInfo`.
    fn new(page_info: &PageInfo, default_value: u8) -> Self {
        Self {
            address: page_info.base_address,
            data: vec![default_value; page_info.size as usize],
        }
    }

    /// Returns the start address of the page.
    pub fn address(&self) -> u64 {
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
    pub(crate) address: u64,
    pub(crate) size: u64,
}

impl FlashSector {
    /// Returns the start address of the sector.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// Returns the size of the sector in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A struct to hold all the information about one region
/// in the flash that is erased during flashing and has to be restored to its original value afterwards.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FlashFill {
    address: u64,
    size: u64,
    page_index: usize,
}

impl FlashFill {
    /// Returns the start address of the fill.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// Returns the size of the fill in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns the corresponding page index of the fill.
    pub fn page_index(&self) -> usize {
        self.page_index
    }
}

/// The built layout of the data in flash.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FlashLayout {
    pub(crate) sectors: Vec<FlashSector>,
    pub(crate) pages: Vec<FlashPage>,
    pub(crate) fills: Vec<FlashFill>,
    data_blocks: Vec<FlashDataBlockSpan>,
}

impl FlashLayout {
    /// Merge another flash layout into this one.
    pub fn merge_from(&mut self, other: FlashLayout) {
        self.sectors.extend(other.sectors);
        self.pages.extend(other.pages);
        self.fills.extend(other.fills);
        self.data_blocks.extend(other.data_blocks);
    }

    /// List of sectors which are erased during flashing.
    pub fn sectors(&self) -> &[FlashSector] {
        &self.sectors
    }

    /// List of pages which are programmed during flashing.
    pub fn pages(&self) -> &[FlashPage] {
        &self.pages
    }

    /// Get the fills of the flash layout.
    ///
    /// This is data which is not written during flashing, but has to be restored to its original value afterwards.
    pub fn fills(&self) -> &[FlashFill] {
        &self.fills
    }

    /// Get the data blocks of the flash layout.
    ///
    /// This is the data which is written during flashing.
    pub fn data_blocks(&self) -> &[FlashDataBlockSpan] {
        &self.data_blocks
    }
}

/// A block of data that is to be written to flash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlashDataBlockSpan {
    address: u64,
    size: u64,
}

impl FlashDataBlockSpan {
    /// Get the start address of the block.
    pub fn address(&self) -> u64 {
        self.address
    }

    /// Returns the size of the block in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// A helper structure to build a flash layout from a set of data blocks.
#[derive(Default)]
pub(super) struct FlashBuilder {
    pub(super) data: BTreeMap<u64, Vec<u8>>,
}

impl FlashBuilder {
    /// Creates a new `FlashBuilder` with empty data.
    pub(super) fn new() -> Self {
        Self {
            data: BTreeMap::new(),
        }
    }

    /// Stages a chunk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub fn add_data(&mut self, address: u64, data: &[u8]) -> Result<(), FlashError> {
        // Ignore zero-length stuff
        if data.is_empty() {
            return Ok(());
        }

        // Check the new data doesn't overlap to the right.
        if let Some((&next_addr, next_data)) = self.data.range(address..).next() {
            if address + (data.len() as u64) > next_addr {
                return Err(FlashError::DataOverlaps {
                    added_addresses: address..address + data.len() as u64,
                    existing_addresses: next_addr..next_addr + next_data.len() as u64,
                });
            }
        }

        // Check the new data doesn't overlap to the left.
        if let Some((&prev_addr, prev_data)) = self.data.range_mut(..address).next_back() {
            let prev_end = prev_addr + (prev_data.len() as u64);

            if prev_end > address {
                return Err(FlashError::DataOverlaps {
                    added_addresses: address..address + data.len() as u64,
                    existing_addresses: prev_addr..prev_addr + prev_data.len() as u64,
                });
            }

            // Optimization: If it exactly touches the left neighbor, extend it instead.
            if prev_end == address {
                prev_data.extend(data);
                return Ok(());
            }
        }

        // Add it
        self.data.insert(address, data.to_vec());

        Ok(())
    }

    /// Check whether there is staged data for a given address range.
    pub(crate) fn has_data_in_range(&self, range: &Range<u64>) -> bool {
        self.data_in_range(range).next().is_some()
    }

    /// Iterate staged data for a given address range.
    ///
    /// Data is returned in ascending address order, and is guaranteed not to overlap.
    /// If a staged chunk is not fully contained in the range, only the contained part is
    /// returned. ie for each returned item (addr, data), it's guaranteed that the condition
    /// `start <= addr && addr + data.len() <= end` upholds.
    pub(crate) fn data_in_range<'s>(
        &'s self,
        range: &Range<u64>,
    ) -> impl Iterator<Item = (u64, &'s [u8])> + use<'s> {
        let range = range.clone();

        let mut adjusted_start = range.start;

        // Check if the immediately preceding data overlaps with the wanted range.
        // If so, adjust the iteration start so it is included.
        if let Some((&prev_addr, prev_data)) = self.data.range(..range.start).next_back() {
            if prev_addr + (prev_data.len() as u64) > range.start {
                adjusted_start = prev_addr;
            }
        }

        self.data
            .range(adjusted_start..range.end)
            .map(move |(&addr, data)| {
                let mut addr = addr;
                let mut data = &data[..];

                // Cut chunk from the left if it starts before `start`.
                if addr < range.start {
                    data = &data[(range.start - addr) as usize..];
                    addr = range.start;
                }

                // Cut chunk from the right if it ends before `end`.
                if addr + (data.len()) as u64 > range.end {
                    data = &data[..(range.end - addr) as usize];
                }

                (addr, data)
            })
    }

    /// Layouts the contents of a flash memory according to the contents of the flash loader.
    pub(super) fn build_sectors_and_pages(
        &self,
        region: &NvmRegion,
        flash_algorithm: &FlashAlgorithm,
        include_empty_pages: bool,
    ) -> Result<FlashLayout, FlashError> {
        let mut layout = FlashLayout::default();

        for info in flash_algorithm.iter_sectors() {
            let range = info.address_range();

            // Ignore the sector if it's outside the NvmRegion.
            if !region.range.contains_range(&range) {
                continue;
            }

            let page = flash_algorithm.page_info(info.base_address).unwrap();
            let page_range = page.address_range();
            let sector_has_data = self.has_data_in_range(&range);
            let page_has_data = self.has_data_in_range(&page_range);

            // Ignore if neither the sector nor the page contain any data.
            if !sector_has_data && !page_has_data {
                continue;
            }

            layout.sectors.push(FlashSector {
                address: info.base_address,
                size: info.size,
            })
        }

        for info in flash_algorithm.iter_pages() {
            let range = info.address_range();

            // Ignore the page if it's outside the NvmRegion.
            if !region.range.contains_range(&range) {
                continue;
            }

            let sector = flash_algorithm.sector_info(info.base_address).unwrap();
            let sector_range = sector.address_range();
            let sector_has_data = self.has_data_in_range(&sector_range);
            let page_has_data = self.has_data_in_range(&range);

            // If include_empty_pages, include the page if there's data in is sector, even if there's no data in the page.
            if !page_has_data && (!include_empty_pages || !sector_has_data) {
                continue;
            }

            let mut page =
                FlashPage::new(&info, flash_algorithm.flash_properties.erased_byte_value);

            let mut fill_start_addr = info.base_address;

            // Loop over all datablocks in the page.
            for (address, data) in self.data_in_range(&range) {
                // Copy data into the page buffer
                let offset = (address - info.base_address) as usize;
                page.data[offset..offset + data.len()].copy_from_slice(data);

                // Fill the hole between the previous data block (or page start if there are no blocks) and current block.
                if address > fill_start_addr {
                    layout.fills.push(FlashFill {
                        address: fill_start_addr,
                        size: address - fill_start_addr,
                        page_index: layout.pages.len(),
                    });
                }
                fill_start_addr = address + data.len() as u64;
            }

            // Fill the hole between the last data block (or page start if there are no blocks) and page end.
            if fill_start_addr < range.end {
                layout.fills.push(FlashFill {
                    address: fill_start_addr,
                    size: range.end - fill_start_addr,
                    page_index: layout.pages.len(),
                });
            }

            layout.pages.push(page);
        }

        for (address, data) in self.data_in_range(&region.range) {
            layout.data_blocks.push(FlashDataBlockSpan {
                address,
                size: data.len() as u64,
            });
        }

        // Return the finished flash layout.
        Ok(layout)
    }
}

#[cfg(test)]
mod tests {
    use probe_rs_target::{FlashProperties, MemoryAccess, SectorDescription};

    use super::*;

    fn assemble_demo_flash1() -> (NvmRegion, FlashAlgorithm) {
        let sd = SectorDescription {
            size: 4096,
            address: 0,
        };

        let flash_algorithm = FlashAlgorithm {
            flash_properties: FlashProperties {
                address_range: 0..1 << 16,
                page_size: 1024,
                erased_byte_value: 255,
                program_page_timeout: 200,
                erase_sector_timeout: 200,
                sectors: vec![sd],
            },
            ..Default::default()
        };

        let region = NvmRegion {
            name: Some("FLASH".into()),
            access: Some(MemoryAccess {
                boot: true,
                ..Default::default()
            }),
            range: 0..1 << 16,
            cores: vec!["main".into()],
            is_alias: false,
        };

        (region, flash_algorithm)
    }

    fn assemble_demo_flash2() -> (NvmRegion, FlashAlgorithm) {
        let sd = SectorDescription {
            size: 128,
            address: 0,
        };

        let flash_algorithm = FlashAlgorithm {
            flash_properties: FlashProperties {
                address_range: 0..1 << 16,
                page_size: 1024,
                erased_byte_value: 255,
                program_page_timeout: 200,
                erase_sector_timeout: 200,
                sectors: vec![sd],
            },
            ..Default::default()
        };

        let region = NvmRegion {
            name: Some("FLASH".into()),
            access: Some(MemoryAccess {
                boot: true,
                ..Default::default()
            }),
            range: 0..1 << 16,
            cores: vec!["main".into()],
            is_alias: false,
        };

        (region, flash_algorithm)
    }

    #[test]
    fn single_byte_in_single_page() {
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                            let mut data = vec![erased_byte_value; 1024];
                            data[0] = 42;
                            data
                        },
                    },
                    FlashPage {
                        address: 0x0400,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![erased_byte_value; 1024],
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![erased_byte_value; 1024],
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                            let mut data = vec![erased_byte_value; 1024];
                            data[0] = 42;
                            data
                        },
                    },
                    FlashPage {
                        address: 0x0800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x0C00,
                        data: vec![erased_byte_value; 1024],
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 1025]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, false)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                            let mut data = vec![erased_byte_value; 1024];
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(42, &[42; 1024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                                *d = erased_byte_value;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x000400,
                        data: {
                            let mut data = vec![erased_byte_value; 1024];
                            for d in &mut data[..42] {
                                *d = 42;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x000800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x000C00,
                        data: vec![erased_byte_value; 1024],
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                            let mut data = vec![erased_byte_value; 1024];
                            for d in &mut data[..928] {
                                *d = 42;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001400,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x001800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: vec![erased_byte_value; 1024],
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
        let (region, flash_algorithm) = assemble_demo_flash1();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

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
                                *d = erased_byte_value;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001400,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x001800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[..692] {
                                *d = erased_byte_value;
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
                                *d = erased_byte_value;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x003400,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x003800,
                        data: vec![erased_byte_value; 1024],
                    },
                    FlashPage {
                        address: 0x003C00,
                        data: vec![erased_byte_value; 1024],
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
                        size: 0x0002B4,
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
        let (region, flash_algorithm) = assemble_demo_flash2();
        let mut flash_builder = FlashBuilder::new();
        flash_builder.add_data(0, &[42; 5024]).unwrap();
        flash_builder.add_data(7860, &[42; 5024]).unwrap();
        let flash_layout = flash_builder
            .build_sectors_and_pages(&region, &flash_algorithm, true)
            .unwrap();

        let erased_byte_value = flash_algorithm.flash_properties.erased_byte_value;

        assert_eq!(
            flash_layout,
            FlashLayout {
                sectors: {
                    let mut sectors = Vec::with_capacity(88);
                    for i in 0..40 {
                        sectors.push(FlashSector {
                            address: 128 * i as u64,
                            size: 0x000080,
                        });
                    }

                    for i in 56..104 {
                        sectors.push(FlashSector {
                            address: 128 * i as u64,
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
                                *d = erased_byte_value;
                            }
                            data
                        },
                    },
                    FlashPage {
                        address: 0x001C00,
                        data: {
                            let mut data = vec![42; 1024];
                            for d in &mut data[..692] {
                                *d = erased_byte_value;
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
                                *d = erased_byte_value;
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
