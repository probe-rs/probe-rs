use super::{FlashBuilder, FlashError, FlashProgress, Flasher};
use crate::config::{MemoryRange, MemoryRegion, RawFlashAlgorithm};
use crate::memory::MemoryInterface;
use crate::session::Session;
use anyhow::anyhow;
use std::{collections::HashMap, ops::Range};

struct RamWrite<'data> {
    address: u32,
    data: &'data [u8],
}

/// Flashable memory types.
#[derive(PartialEq, Debug)]
pub(super) enum MemoryType {
    /// Non-volatile memory, e.g. flash or EEPROM.
    Nvm,
    /// RAM.
    Ram,
}

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
/// Use `add_data()` to add a chunks of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are
/// contiguous and the same flash algorithm can be used for all of them.
pub(super) struct FlashLoader<'mmap, 'algos, 'data> {
    memory_map: &'mmap [MemoryRegion],
    flash_algorithms: &'algos [RawFlashAlgorithm],
    builders: HashMap<Range<u32>, FlashBuilder<'data>>,
    ram_write: Vec<RamWrite<'data>>,
    keep_unwritten: bool,
}

impl<'mmap, 'algos, 'data> FlashLoader<'mmap, 'algos, 'data> {
    pub(super) fn new(
        memory_map: &'mmap [MemoryRegion],
        flash_algorithms: &'algos [RawFlashAlgorithm],
        keep_unwritten: bool,
    ) -> Self {
        Self {
            memory_map,
            flash_algorithms,
            builders: HashMap::new(),
            ram_write: Vec::new(),
            keep_unwritten,
        }
    }

    /// Stages a chunk of data to be programmed.
    ///
    /// Region crossing data chunks are allowed as long as the regions are
    /// contiguous and the same flash algorithm can be used for all of them.
    pub(super) fn add_data(
        &mut self,
        mut address: u32,
        data: &'data [u8],
    ) -> Result<(), FlashError> {
        let data_size = data.len();
        let mut remaining = data_size;
        let base_address = address;
        println!("---");
        while remaining > 0 {
            // Get the flash region in which this chunk of data starts.
            let (memory_range, memory_type) =
                self.get_region_for_address(address).ok_or_else(|| {
                    FlashError::NoSuitableMemoryRegion {
                        start: base_address,
                        end: base_address + data.len() as u32,
                    }
                })?;

            // Determine how much more data can be contained by this region.
            let program_length = usize::min(remaining, (memory_range.end - address) as usize);

            // If we found a corresponding region, create a builder.
            println!("Data size: {}", data_size);
            println!("Remaining: {}", remaining);
            println!("Length: {}", program_length);
            let data_range = (data_size - remaining)..(data_size - remaining + program_length);
            println!("Range: {:?}", data_range);
            let data_slice = &data[data_range];
            println!("==> Write {} bytes at {}", data_slice.len(), address);
            match memory_type {
                MemoryType::Nvm => {
                    // Add as much data to the builder as can be contained by this region.
                    self.builders
                        .entry(memory_range)
                        .or_insert_with(FlashBuilder::new)
                        .add_data(address, data_slice)?;
                }
                MemoryType::Ram => {
                    // Add data to be written to the vector.
                    self.ram_write.push(RamWrite {
                        address,
                        data: data_slice,
                    });
                }
            }

            // Advance the cursors.
            remaining -= program_length;
            address += program_length as u32
        }
        Ok(())
    }

    /// Find the appropriate memory region for that address.
    pub(super) fn get_region_for_address(&self, address: u32) -> Option<(Range<u32>, MemoryType)> {
        // Look through the flash algorithms, check whether we know of a flash
        // algorithm that can flash the specified address.
        let flashable_range = self
            .flash_algorithms
            .iter()
            .map(|algo| &algo.flash_properties.address_range)
            .filter(|range| range.contains(&address))
            .next();
        if let Some(range) = flashable_range {
            return Some((range.clone(), MemoryType::Nvm));
        }

        // If no flash algorithm could be found for that range, check whether a
        // RAM memory region could match (since that's supported as well by the
        // flash loader).
        self.memory_map
            .iter()
            .filter_map(|mmap| {
                if let MemoryRegion::Ram(region) = mmap {
                    if region.range.contains(&address) {
                        return Some(region.range.clone());
                    }
                }
                None
            })
            .map(|range| (range, MemoryType::Ram))
            .next()
    }

    /// Writes all the stored data chunks to flash.
    ///
    /// Requires a session with an attached target that has a known flash algorithm.
    ///
    /// If `do_chip_erase` is `true` the entire flash will be erased.
    pub(super) fn commit(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        do_chip_erase: bool,
    ) -> Result<(), FlashError> {
        // Iterate over builders we've created and program the data.
        for (range, builder) in &self.builders {
            log::debug!(
                "Using builder for region (0x{:08x}..0x{:08x})",
                range.start,
                range.end
            );

            // Try to find a flash algorithm for the range of the current builder
            for algorithm in session.flash_algorithms() {
                log::debug!(
                    "Algorithm {} - start: {:#08x} - size: {:#08x}",
                    algorithm.name,
                    algorithm.flash_properties.address_range.start,
                    algorithm.flash_properties.address_range.end
                        - algorithm.flash_properties.address_range.start
                );
            }

            let algorithms = session.flash_algorithms();
            let algorithms = algorithms
                .iter()
                .filter(|fa| fa.flash_properties.address_range.contains_range(range))
                .collect::<Vec<_>>();

            log::debug!("Algorithms: {:?}", &algorithms);

            let raw_flash_algorithm = match algorithms.len() {
                0 => {
                    return Err(FlashError::NoFlashLoaderAlgorithmAttached);
                }
                1 => &algorithms[0],
                _ => algorithms
                    .iter()
                    .find(|a| a.default)
                    .ok_or(FlashError::NoFlashLoaderAlgorithmAttached)?,
            };

            let mm = session.memory_map();
            let ram = mm
                .iter()
                .find_map(|mm| match mm {
                    MemoryRegion::Ram(ram) => Some(ram),
                    _ => None,
                })
                .ok_or_else(|| anyhow!("No RAM defined for chip."))?;

            let flash_algorithm = raw_flash_algorithm.assemble(ram, session.architecture())?;

            // Program the data.
            let mut flasher = Flasher::new(session, flash_algorithm, range.clone());
            flasher.program(builder, do_chip_erase, self.keep_unwritten, false, progress)?
        }

        // Write data to ram.

        // Attach to memory and core.
        let mut core = session.core(0).map_err(FlashError::Memory)?;

        for RamWrite { address, data } in &self.ram_write {
            log::info!(
                "Ram write program data @ {:X} {} bytes",
                *address,
                data.len()
            );
            // Write data to memory.
            core.write_8(*address, data).map_err(FlashError::Memory)?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::default::Default;

    use crate::config::{FlashProperties, GenericRegion, RamRegion};

    /// Create a vector of flash algorithms for the following NVM memory regions:
    ///
    /// - NVM: 10..50
    /// - NVM: 50..60
    /// - NVM: 120..200
    fn make_flash_algorithms() -> Vec<RawFlashAlgorithm> {
        vec![
            RawFlashAlgorithm {
                flash_properties: FlashProperties {
                    address_range: 10..50,
                    ..Default::default()
                },
                ..Default::default()
            },
            RawFlashAlgorithm {
                flash_properties: FlashProperties {
                    address_range: 50..60,
                    ..Default::default()
                },
                ..Default::default()
            },
            RawFlashAlgorithm {
                flash_properties: FlashProperties {
                    address_range: 120..200,
                    ..Default::default()
                },
                ..Default::default()
            },
        ]
    }

    /// Create a memory map with the following regions:
    ///
    /// - RAM: 1000..2000
    /// - Generic: 2000..3000
    /// - RAM: 4000..4800
    /// - RAM: 4800..5000
    fn make_memory_map() -> Vec<MemoryRegion> {
        vec![
            MemoryRegion::Ram(RamRegion {
                range: 1000..2000,
                is_boot_memory: false,
            }),
            MemoryRegion::Generic(GenericRegion { range: 2000..3000 }),
            MemoryRegion::Ram(RamRegion {
                range: 4000..4800,
                is_boot_memory: false,
            }),
            MemoryRegion::Ram(RamRegion {
                range: 4800..5000,
                is_boot_memory: false,
            }),
        ]
    }

    #[test]
    fn get_region_for_address() {
        // Generate test loader
        let memory_map = make_memory_map();
        let flash_algorithms = make_flash_algorithms();
        let loader = FlashLoader::new(&memory_map, &flash_algorithms, true);

        /// Assert that the specified address is within the specified region.
        macro_rules! assert_in_region {
            ($address:expr, $region:expr, $type:expr) => {{
                assert_eq!(
                    loader.get_region_for_address($address),
                    Some(($region, $type))
                );
            }};
        }

        // NVM: at the start of a region
        assert_in_region!(10, 10..50, MemoryType::Nvm);
        // NVM: in the middle of a region
        assert_in_region!(42, 10..50, MemoryType::Nvm);
        // NVM: at the end of the region
        assert_in_region!(199, 120..200, MemoryType::Nvm);
        // RAM: in the middle of the first region
        assert_in_region!(1500, 1000..2000, MemoryType::Ram);
        // RAM: in the middle of the second region
        assert_in_region!(4500, 4000..4800, MemoryType::Ram);
        // Generic memory address, neither RAM nor NVM
        assert_eq!(loader.get_region_for_address(2222), None);
        // Outside any valid region
        assert_eq!(loader.get_region_for_address(200), None);
    }

    #[test]
    fn add_data() {
        // Generate test loader
        let memory_map = make_memory_map();
        let flash_algorithms = make_flash_algorithms();

        // Add data in an NVM region.
        let mut loader = FlashLoader::new(&memory_map, &flash_algorithms, true);
        loader.add_data(20, &[0x42; 10]).unwrap();

        // Add data in a RAM region.
        let mut loader = FlashLoader::new(&memory_map, &flash_algorithms, true);
        loader.add_data(4500, &[0x23; 10]).unwrap();

        // Add data across two contiguous RAM memory regions.
        let mut loader = FlashLoader::new(&memory_map, &flash_algorithms, true);
        loader.add_data(4700, &[0x23; 200]).unwrap();

        macro_rules! assert_error {
            ($addr:expr, $data:expr) => {{
                let mut loader = FlashLoader::new(&memory_map, &flash_algorithms, true);
                let result = loader.add_data($addr, $data);
                if let Err(FlashError::NoSuitableMemoryRegion { start, end }) = result {
                    assert_eq!(start, $addr, "Start address in error struct is wrong");
                    assert_eq!(
                        end,
                        $addr + $data.len() as u32,
                        "End address in error struct is wrong"
                    );
                } else {
                    panic!("Expected NoSuitableMemoryRegion, got {:?}", result);
                }
            }};
        };

        // Add data that goes outside an NVM region, this should trigger an
        // error.
        assert_error!(50, &[0x01; 11]);

        // Add data that overlaps two contiguous NVM regions with different
        // flash algorithms, this should trigger an error.
        assert_error!(40, &[0x01; 15]);

        // Add data that overlaps between contiguous RAM / generic memory
        // regions, this should trigger an error.
        assert_error!(1990, &[0x01; 20]);
    }
}
