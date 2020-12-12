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
        let size = data.len();
        let mut remaining = size;
        while remaining > 0 {
            // Get the flash region in which this chunk of data starts.
            let (range, memory_type) = self.get_region_for_address(address).ok_or_else(|| {
                FlashError::NoSuitableMemoryRegion {
                    start: address,
                    end: address + data.len() as u32,
                }
            })?;

            // Determine how much more data can be contained by this region.
            let program_length = usize::min(remaining, (range.end - address + 1) as usize);

            // If we found a corresponding region, create a builder.
            match memory_type {
                MemoryType::Nvm => {
                    // Add as much data to the builder as can be contained by this region.
                    self.builders
                        .entry(range)
                        .or_insert_with(FlashBuilder::new)
                        .add_data(address, &data[size - remaining..program_length])?;
                }
                MemoryType::Ram => {
                    // Add data to be written to the vector.
                    let data = &data[size - remaining..program_length];
                    self.ram_write.push(RamWrite { address, data });
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
                    Some(region.range.clone())
                } else {
                    None
                }
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
