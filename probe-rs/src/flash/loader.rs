use crate::session::Session;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use super::builder::FlashBuilder;
use super::flasher::Flasher;
use super::FlashProgress;
use crate::config::memory::MemoryRange;
use crate::config::memory::{FlashRegion, MemoryRegion};

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
/// Use `add_data()` to add a chunks of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are contiguous.
pub struct FlashLoader<'a, 'b> {
    memory_map: &'a [MemoryRegion],
    builders: HashMap<FlashRegion, FlashBuilder<'b>>,
    keep_unwritten: bool,
}

#[derive(Debug)]
pub enum FlashLoaderError {
    NoSuitableFlash(u32),      // Contains the faulty address.
    MemoryRegionNotFlash(u32), // Contains the faulty address.
    NoFlashLoaderAlgorithmAttached,
}

impl Error for FlashLoaderError {}

impl fmt::Display for FlashLoaderError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        use FlashLoaderError::*;

        match self {
            NoSuitableFlash(addr) => write!(f, "No flash memory was found at address {:#08x}.", addr),
            MemoryRegionNotFlash(addr) => write!(f, "Trying to access flash at address {:#08x}, which is not inside any defined flash region.", addr),
            NoFlashLoaderAlgorithmAttached => write!(f, "Trying to write flash, but no flash loader algorithm is attached."),
        }
    }
}

impl<'a, 'b> FlashLoader<'a, 'b> {
    pub fn new(memory_map: &'a [MemoryRegion], keep_unwritten: bool) -> Self {
        Self {
            memory_map,
            builders: HashMap::new(),
            keep_unwritten,
        }
    }
    /// Stages a junk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub fn add_data(&mut self, mut address: u32, data: &'b [u8]) -> Result<(), FlashLoaderError> {
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
                return Err(FlashLoaderError::NoSuitableFlash(address));
            }
        }
        Ok(())
    }

    pub fn get_region_for_address(
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
    pub fn commit(
        &mut self,
        session: &mut Session,
        progress: &FlashProgress,
        do_chip_erase: bool,
    ) -> Result<(), FlashLoaderError> {
        let target = &session.target;
        let probe = &mut session.probe;

        // Iterate over builders we've created and program the data.
        for (region, builder) in &self.builders {
            log::debug!(
                "Using builder for regioon (0x{:08x}..0x{:08x})",
                region.range.start,
                region.range.end
            );

            // Try to find a flash algorithm for the range of the current builder
            for algorithm in &target.flash_algorithms {
                log::debug!(
                    "Algorithm {} - start: {:#08x} - size: {:#08x}",
                    algorithm.name,
                    algorithm.flash_properties.range.start,
                    algorithm.flash_properties.range.end - algorithm.flash_properties.range.start
                );
            }

            let algorithms: Vec<_> = target
                .flash_algorithms
                .iter()
                .filter(|fa| fa.flash_properties.range.contains_range(&region.range))
                .collect();

            //log::debug!("Algorithms: {:?}", &algorithms);

            let raw_flash_algorithm = match algorithms.len() {
                0 => {
                    return Err(FlashLoaderError::NoFlashLoaderAlgorithmAttached);
                }
                1 => &algorithms[0],
                _ => algorithms
                    .iter()
                    .find(|a| a.default)
                    .ok_or(FlashLoaderError::NoFlashLoaderAlgorithmAttached)?,
            };

            let ram = target
                .memory_map
                .iter()
                .find(|mm| match mm {
                    MemoryRegion::Ram(_) => true,
                    _ => false,
                })
                .expect("No RAM defined for chip.");

            let unwrapped_ram = match ram {
                MemoryRegion::Ram(ram) => ram,
                _ => unreachable!(),
            };

            let flash_algorithm = raw_flash_algorithm.assemble(unwrapped_ram);

            // Program the data.
            builder
                .program(
                    Flasher::new(target, probe, &flash_algorithm, region),
                    do_chip_erase,
                    self.keep_unwritten,
                    progress,
                )
                .unwrap();
        }

        Ok(())
    }
}
