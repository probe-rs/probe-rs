use crate::session::Session;
use std::collections::HashMap;

use super::*;

pub struct Ranges<I: Iterator<Item=usize> + Sized> {
    list: I,
    start_item: Option<usize>,
    last_item: Option<usize>
}

impl<I: Iterator<Item=usize> + Sized> Ranges<I> {
    pub fn new(list: I) -> Self {
        Self {
            list,
            start_item: None,
            last_item: Some(usize::max_value() - 1)
        }
    }
}

impl<I: Iterator<Item=usize> + Sized> Iterator for Ranges<I> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<(usize, usize)> {
        let r;
        if self.start_item.is_none() {
            self.start_item = self.list.next();
            self.last_item = self.start_item;
        }
        loop {
            if let Some(item) = self.list.next() {
                if item == self.last_item.unwrap() + 1 {
                    self.last_item = Some(item);
                } else {
                    r = (self.start_item.unwrap(), self.last_item.unwrap());
                    self.last_item = Some(item);
                    self.start_item = self.last_item;
                    break;
                }
            } else {
                if let Some(last_item) = self.last_item {
                    self.last_item = None;
                    return Some((self.start_item.unwrap(), last_item));
                } else {
                    return None;
                }
            }
        }

        Some(r)
    }
}

/// Accepts a sorted list of byte addresses. Breaks the addresses into contiguous ranges.
/// Yields 2-tuples of the start and end address for each contiguous range.

/// For instance, the input [0, 1, 2, 3, 32, 33, 34, 35] will yield the following 2-tuples:
/// (0, 3) and (32, 35).
pub fn ranges<I: Iterator<Item = usize>>(list: I)-> Ranges<I> {
    Ranges::new(list)
}

/// Handles high level programming of raw binary data to flash.
/// 
/// If you need file programming, either binary files or other formats, please see the
/// FileProgrammer class.
/// 
/// This manager provides a simple interface to programming flash that may cross flash
/// region boundaries. To use it, create an instance and pass in the session object. Then call
/// add_data() for each chunk of binary data you need to write. When all data is added, call the
/// commit() method to write everything to flash. You may reuse a single FlashLoader instance for
/// multiple add-commit sequences.
/// 
/// When programming across multiple regions, progress reports are combined so that only a
/// one progress output is reported. Similarly, the programming performance report for each region
/// is suppresed and a combined report is logged.
/// 
/// Internally, FlashBuilder is used to optimize programming within each memory region.
pub struct FlashLoader<'a, 'b> {
    memory_map: &'a Vec<MemoryRegion>,
    builders: HashMap<FlashRegion, FlashBuilder<'b>>,
    total_data_size: usize,
    chip_erase: bool,
    smart_flash: bool,
    trust_crc: bool,
    keep_unwritten: bool,
}

#[derive(Debug)]
pub enum FlashLoaderError {
    MemoryRegionNotDefined(u32), // Contains the faulty address.
    MemoryRegionNotFlash(u32) // Contains the faulty address.
}

impl<'a, 'b> FlashLoader<'a, 'b> {
    pub fn new(
        memory_map: &'a Vec<MemoryRegion>,
        smart_flash: bool,
        trust_crc: bool,
        keep_unwritten: bool
    ) -> Self {
        Self {
            memory_map,
            builders: HashMap::new(),
            total_data_size: 0,
            chip_erase: false,
            smart_flash,
            trust_crc,
            keep_unwritten,
        }
    }
    
    /// Clear all state variables.
    fn reset_state(&mut self) {
        self.builders = HashMap::new();
        self.total_data_size = 0;
    }
    
    /// Add a chunk of data to be programmed.
    ///
    /// The data may cross flash memory region boundaries, as long as the regions are contiguous.
    /// `address` is the address where the first byte of `data` is located.
    /// `data` is an iterator of u8 bytes to be written at given `address` and onwards.
    pub fn add_data(& mut self, mut address: u32, data: &'b [u8]) -> Result<(), FlashLoaderError> {
        let size = data.len();
        let mut remaining = size;
        while remaining > 0 {
            // Look up flash region.
            let possible_region = Self::get_region_for_address(self.memory_map, address);
            if let Some(region) = possible_region {
                match region {
                    MemoryRegion::Flash(region) => {
                        // Get our builder instance.
                        if !self.builders.contains_key(&region) {
                            // if region.flash is None:
                            //     raise RuntimeError("flash memory region at address 0x%08x has no flash instance" % address)
                            self.builders.insert(region.clone(), FlashBuilder::new(region.range.start));
                        };
                    
                        // Add as much data to the builder as is contained by this region.
                        let program_length = usize::min(remaining, (region.range.end - address + 1) as usize);
                        self.builders.get_mut(&region).map(|r| r.add_data(address, &data[size - remaining..program_length]));
                        
                        // Advance the cursors.
                        remaining -= program_length;
                        address += program_length as u32;
                    },
                    _ => {
                        return Err(FlashLoaderError::MemoryRegionNotFlash(address));
                    }
                }
            } else {
                return Err(FlashLoaderError::MemoryRegionNotDefined(address));
            }
        }
        Ok(())
    }

    pub fn get_region_for_address(
        memory_map: &Vec<MemoryRegion>,
        address: u32
    ) -> Option<&MemoryRegion> {
        for region in memory_map {
            let r = match region {
                MemoryRegion::Ram(r) => r.range.clone(),
                MemoryRegion::Rom(r) => r.range.clone(),
                MemoryRegion::Flash(r) => r.range.clone(),
                MemoryRegion::Device(r) => r.range.clone()
            };
            if r.contains(&address) {
                return Some(region);
            }
        }
        None
    }
    
    /// Write all collected data to flash.
        
    /// This routine ensures that chip erase is only used once if either the auto mode or chip
    /// erase mode are used. As an example, if two regions are to be written to and True was
    /// passed to the constructor for chip_erase (or if the session option was set), then only
    /// the first region will actually use chip erase. The second region will be forced to use
    /// sector erase. This will not result in extra erasing, as sector erase always verifies whether
    /// the sectors are already erased. This will, of course, also work correctly if the flash
    /// algorithm for the first region doesn't actually erase the entire chip (all regions).
    
    /// After calling this method, the loader instance can be reused to program more data.
    pub fn commit(&mut self, session: &mut Session) {
        let mut did_chip_erase = false;
        
        // Iterate over builders we've created and program the data.
        let mut builders: Vec<(&FlashRegion, &FlashBuilder)> = self.builders.iter().collect();
        builders.sort_unstable_by_key(|v| v.1.flash_start);
        let sorted = builders;
        for builder in sorted {
            // Program the data.
            let chip_erase = Some(if !did_chip_erase { self.chip_erase } else { false });
            builder.1.program(
                Flasher::new(session, builder.0),
                chip_erase,
                self.smart_flash,
                self.trust_crc,
                self.keep_unwritten
            );
            did_chip_erase = true;
        }

        // Clear state to allow reuse.
        self.reset_state();
    }
}

#[test]
fn ranges_works() {
    let r = ranges([0, 1, 3, 5, 6, 7].iter().cloned());
    assert_eq!(
        r.collect::<Vec<(usize, usize)>>(),
        vec![
            (0, 1),
            (3, 3),
            (5, 7),
        ]
    );

    let r = ranges([3, 4, 7, 9, 11, 12].iter().cloned());
    assert_eq!(
        r.collect::<Vec<(usize, usize)>>(),
        vec![
            (3, 4),
            (7, 7),
            (9, 9),
            (11, 12),
        ]
    );

    let r = ranges([1, 3, 5, 7].iter().cloned());
    assert_eq!(
        r.collect::<Vec<(usize, usize)>>(),
        vec![
            (1, 1),
            (3, 3),
            (5, 5),
            (7, 7),
        ]
    );
}