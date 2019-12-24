use crate::session::Session;
use std::collections::HashMap;
use std::error::Error;
use std::fmt;

use super::builder::FlashBuilder;
use super::flasher::Flasher;
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
        progress: std::sync::Arc<std::sync::RwLock<FlashProgress>>,
        do_chip_erase: bool,
    ) -> Result<(), FlashLoaderError> {
        let target = &session.target;
        let probe = &mut session.probe;

        // If the session target has a flash algorithm attached, initiate the download.
        if let Some(flash_algorithm) = target.flash_algorithm.as_ref() {
            // Iterate over builders we've created and program the data.
            for (region, builder) in &self.builders {
                log::debug!(
                    "Using builder for region (0x{:08x}..0x{:08x})",
                    region.range.start,
                    region.range.end
                );
                // Program the data.
                builder
                    .program(
                        Flasher::new(target, probe, flash_algorithm, region),
                        do_chip_erase,
                        self.keep_unwritten,
                        progress.clone(),
                    )
                    .unwrap();
            }

            Ok(())
        } else {
            Err(FlashLoaderError::NoFlashLoaderAlgorithmAttached)
        }
    }
}

#[derive(Default)]
pub struct FlashProgress {
    total_sectors: usize,
    total_pages: usize,
    erased_sectors: usize,
    programmed_pages: usize,
    total_time: u128,
}

impl FlashProgress {
    pub fn new() -> Self {
        Self {
            total_sectors: 0,
            total_pages: 0,
            erased_sectors: 0,
            programmed_pages: 0,
            total_time: 0,
        }
    }

    pub fn set_goal(&mut self, total_sectors: usize, total_pages: usize) {
        self.total_sectors = total_sectors;
        self.total_pages = total_pages;
    }

    pub fn increment_erased_sectors(&mut self) {
        self.erased_sectors += 1;
    }

    pub fn increment_programmed_pages(&mut self) {
        self.programmed_pages += 1;
    }

    pub fn add_time(&mut self, delta: u128) {
        self.total_time += delta;
    }

    pub fn done(&self) -> usize {
        self.programmed_pages + self.erased_sectors
    }

    pub fn total(&self) -> usize {
        self.total_sectors + self.total_pages
    }
}