use super::{FlashBuilder, FlashError, FlashProgress, Flasher};
use crate::config::{FlashRegion, MemoryRange, MemoryRegion};
use crate::session::Session;
use anyhow::anyhow;
use std::collections::HashMap;

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
/// Use `add_data()` to add a chunks of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are contiguous.
pub(super) struct FlashLoader<'mmap, 'data> {
    memory_map: &'mmap [MemoryRegion],
    builders: HashMap<FlashRegion, FlashBuilder<'data>>,
    keep_unwritten: bool,
}

impl<'mmap, 'data> FlashLoader<'mmap, 'data> {
    pub(super) fn new(memory_map: &'mmap [MemoryRegion], keep_unwritten: bool) -> Self {
        Self {
            memory_map,
            builders: HashMap::new(),
            keep_unwritten,
        }
    }
    /// Stages a chunk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub(super) fn add_data(
        &mut self,
        mut address: u32,
        data: &'data [u8],
    ) -> Result<(), FlashError> {
        let size = data.len();
        let mut remaining = size;
        while remaining > 0 {
            // Get the flash region in with this chunk of data starts.
            let possible_region = Self::get_region_for_address(self.memory_map, address);
            // If we found a corresponding region, create a builder.
            if let Some(MemoryRegion::Flash(region)) = possible_region {
                // Get our builder instance.
                if !self.builders.contains_key(region) {
                    self.builders.insert(region.clone(), FlashBuilder::new());
                };

                // Determine how much more data can be contained by this region.
                let program_length =
                    usize::min(remaining, (region.range.end - address + 1) as usize);

                // Add as much data to the builder as can be contained by this region.
                self.builders
                    .get_mut(&region)
                    .map(|r| r.add_data(address, &data[size - remaining..program_length]));

                // Advance the cursors.
                remaining -= program_length;
                address += program_length as u32;
            } else {
                return Err(FlashError::NoSuitableFlash {
                    start: address,
                    end: address + data.len() as u32,
                });
            }
        }
        Ok(())
    }

    pub(super) fn get_region_for_address(
        memory_map: &[MemoryRegion],
        address: u32,
    ) -> Option<&MemoryRegion> {
        for region in memory_map {
            let r = match region {
                MemoryRegion::Ram(r) => r.range.clone(),
                MemoryRegion::Flash(r) => r.range.clone(),
                MemoryRegion::Generic(r) => r.range.clone(),
            };
            if r.contains(&address) {
                return Some(region);
            }
        }
        None
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
        for (region, builder) in &self.builders {
            log::debug!(
                "Using builder for region (0x{:08x}..0x{:08x})",
                region.range.start,
                region.range.end
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
                .filter(|fa| {
                    fa.flash_properties
                        .address_range
                        .contains_range(&region.range)
                })
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
            let mut flasher = Flasher::new(session, flash_algorithm, region.clone());
            flasher.program(builder, do_chip_erase, self.keep_unwritten, false, progress)?
        }

        Ok(())
    }
}
