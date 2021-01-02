use super::{ExtractedFlashData, FlashBuilder, FlashError, FlashProgress, Flasher};
use crate::config::{MemoryRange, MemoryRegion, NvmRegion, TargetDescriptionSource};
use crate::memory::MemoryInterface;
use crate::session::Session;
use std::collections::HashMap;

struct RamWrite<'data> {
    address: u32,
    data: &'data [u8],
}

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
/// Use `add_data()` to add a chunks of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are contiguous.
pub struct FlashLoader<'data> {
    memory_map: Vec<MemoryRegion>,
    builders: HashMap<NvmRegion, FlashBuilder<'data>>,
    ram_write: Vec<RamWrite<'data>>,
    keep_unwritten: bool,

    /// Source of the flash description,
    /// used for diagnostics.
    source: TargetDescriptionSource,
}

impl<'mmap, 'data> FlashLoader<'data> {
    /// Create a new flash loader.
    pub fn new(
        memory_map: Vec<MemoryRegion>,
        keep_unwritten: bool,
        source: TargetDescriptionSource,
    ) -> Self {
        Self {
            memory_map,
            builders: HashMap::new(),
            ram_write: Vec::new(),
            keep_unwritten,
            source,
        }
    }

    pub(crate) fn add_section(
        &mut self,
        data: ExtractedFlashData<'data>,
    ) -> Result<(), FlashError> {
        log::debug!("Adding data: {:x?}", data);

        self.add_data_internal(data)
    }

    /// Stages a chunk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub fn add_data(&mut self, address: u32, data: &'data [u8]) -> Result<(), FlashError> {
        let data = ExtractedFlashData::from_unknown_source(address, data);
        self.add_data_internal(data)
    }

    fn add_data_internal(&mut self, mut data: ExtractedFlashData<'data>) -> Result<(), FlashError> {
        log::debug!(
            "Adding data at address {:#010x} with size {} bytes",
            data.address(),
            data.len()
        );

        while data.len() > 0 {
            // Get the flash region in with this chunk of data starts.
            let possible_region = Self::get_region_for_address(&self.memory_map, data.address());
            // If we found a corresponding region, create a builder.
            match possible_region {
                Some(MemoryRegion::Nvm(region)) => {
                    // Get our builder instance.
                    if !self.builders.contains_key(region) {
                        self.builders.insert(region.clone(), FlashBuilder::new());
                    };

                    // Determine how much more data can be contained by this region.
                    let program_length =
                        usize::min(data.len(), (region.range.end - data.address() + 1) as usize);

                    let programmed_data = data.split_off(program_length);

                    // Add as much data to the builder as can be contained by this region.
                    self.builders
                        .get_mut(&region)
                        .map(|r| r.add_data(programmed_data.address(), programmed_data.data()));
                }
                Some(MemoryRegion::Ram(region)) => {
                    // Determine how much more data can be contained by this region.
                    let program_length =
                        usize::min(data.len(), (region.range.end - data.address() + 1) as usize);

                    let programmed_data = data.split_off(program_length);

                    // Add data to be written to the vector.
                    self.ram_write.push(RamWrite {
                        address: programmed_data.address(),
                        data: programmed_data.data(),
                    });
                }
                _ => {
                    return Err(FlashError::NoSuitableNvm {
                        start: data.address(),
                        end: data.address() + data.len() as u32,
                        description_source: self.source.clone(),
                    })
                }
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
                MemoryRegion::Nvm(r) => r.range.clone(),
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
        dry_run: bool,
    ) -> Result<(), FlashError> {
        // Iterate over builders we've created and program the data.
        for (region, builder) in &self.builders {
            log::debug!(
                "Using builder for region (0x{:08x}..0x{:08x})",
                region.range.start,
                region.range.end
            );

            // Try to find a flash algorithm for the range of the current builder
            let algorithms = &session.target().flash_algorithms;

            for algorithm in algorithms {
                log::debug!(
                    "Algorithm {} - start: {:#08x} - size: {:#08x}",
                    algorithm.name,
                    algorithm.flash_properties.address_range.start,
                    algorithm.flash_properties.address_range.end
                        - algorithm.flash_properties.address_range.start
                );
            }
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

            let mm = &session.target().memory_map;
            let ram = mm
                .iter()
                .find_map(|mm| match mm {
                    MemoryRegion::Ram(ram) => Some(ram),
                    _ => None,
                })
                .ok_or(FlashError::NoRamDefined {
                    chip: session.target().name.clone(),
                })?;

            let flash_algorithm = raw_flash_algorithm.assemble(ram, session.target())?;

            if dry_run {
                println!("Skipping programming, dry run!");
                progress.failed_erasing();
                continue;
            }

            // Program the data.
            let mut flasher = Flasher::new(session, flash_algorithm, region.clone());
            flasher.program(builder, do_chip_erase, self.keep_unwritten, false, progress)?
        }

        // Write data to ram.

        // Attach to memory and core.
        let mut core = session.core(0).map_err(FlashError::Core)?;

        for RamWrite { address, data } in &self.ram_write {
            log::info!(
                "Ram write program data @ {:X} {} bytes",
                *address,
                data.len()
            );
            // Write data to memory.
            core.write_8(*address, data).map_err(FlashError::Core)?;
        }

        Ok(())
    }
}
