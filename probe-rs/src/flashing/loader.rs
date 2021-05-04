use ihex::Record;

use super::{
    extract_from_elf, BinOptions, DownloadOptions, ExtractedFlashData, FileDownloadError,
    FlashAlgorithm, FlashBuilder, FlashError, FlashProgress, Flasher,
};
use crate::config::{MemoryRange, MemoryRegion, NvmRegion, TargetDescriptionSource};
use crate::memory::MemoryInterface;
use crate::session::Session;
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
};

struct RamWrite {
    address: u32,
    data: Vec<u8>,
}

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
/// Use `add_data()` to add a chunks of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are contiguous.
pub struct FlashLoader {
    memory_map: Vec<MemoryRegion>,
    builders: HashMap<NvmRegion, FlashBuilder>,
    ram_write: Vec<RamWrite>,

    /// Source of the flash description,
    /// used for diagnostics.
    source: TargetDescriptionSource,
}

impl FlashLoader {
    /// Create a new flash loader.
    pub fn new(memory_map: Vec<MemoryRegion>, source: TargetDescriptionSource) -> Self {
        Self {
            memory_map,
            builders: HashMap::new(),
            ram_write: Vec::new(),
            source,
        }
    }

    /// Stages a chunk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub fn add_data(&mut self, address: u32, data: &[u8]) -> Result<(), FlashError> {
        let data = ExtractedFlashData::from_unknown_source(address, data);
        self.add_data_internal(data)
    }

    fn add_data_internal(&mut self, mut data: ExtractedFlashData) -> Result<(), FlashError> {
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
                        usize::min(data.len(), (region.range.end - data.address()) as usize);

                    let programmed_data = data.split_off(program_length);

                    // Add as much data to the builder as can be contained by this region.
                    self.builders
                        .get_mut(&region)
                        .map(|r| r.add_data(programmed_data.address(), programmed_data.data()));
                }
                Some(MemoryRegion::Ram(region)) => {
                    // Determine how much more data can be contained by this region.
                    let program_length =
                        usize::min(data.len(), (region.range.end - data.address()) as usize);

                    let programmed_data = data.split_off(program_length);

                    // Add data to be written to the vector.
                    self.ram_write.push(RamWrite {
                        address: programmed_data.address(),
                        data: programmed_data.data().to_vec(),
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

    /// Reads the data from the binary file and adds it to the loader without splitting it into flash instructions yet.
    pub fn load_bin_data<T: Read + Seek>(
        &mut self,
        file: &mut T,
        options: BinOptions,
    ) -> Result<(), FileDownloadError> {
        // Skip the specified bytes.
        file.seek(SeekFrom::Start(u64::from(options.skip)))?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        self.add_data(
            if let Some(address) = options.base_address {
                address
            } else {
                // If no base address is specified use the start of the boot memory.
                // TODO: Implement this as soon as we know targets.
                0
            },
            &buf,
        )?;

        Ok(())
    }

    /// Reads the HEX data segments and adds them as loadable data blocks to the loader.
    /// This does not create and flash loader instructions yet.
    pub fn load_hex_data<T: Read + Seek>(&mut self, file: &mut T) -> Result<(), FileDownloadError> {
        let mut base_address = 0;

        let mut data = String::new();
        file.read_to_string(&mut data)?;

        for record in ihex::Reader::new(&data) {
            let record = record?;
            use Record::*;
            match record {
                Data { offset, value } => {
                    let offset = base_address + offset as u32;
                    self.add_data(offset, &value)?;
                }
                EndOfFile => (),
                ExtendedSegmentAddress(address) => {
                    base_address = (address as u32) * 16;
                }
                StartSegmentAddress { .. } => (),
                ExtendedLinearAddress(address) => {
                    base_address = (address as u32) << 16;
                }
                StartLinearAddress(_) => (),
            };
        }
        Ok(())
    }

    /// Prepares the data sections that have to be loaded into flash from an ELF file.
    /// This will validate the ELF file and transform all its data into sections but no flash loader commands yet.
    pub fn load_elf_data<T: Read>(&mut self, file: &mut T) -> Result<(), FileDownloadError> {
        let mut elf_buffer = Vec::new();
        file.read_to_end(&mut elf_buffer)?;

        let mut extracted_data = Vec::new();

        let num_sections = extract_from_elf(&mut extracted_data, &mut elf_buffer)?;

        if num_sections == 0 {
            log::warn!("No loadable segments were found in the ELF file.");
            return Err(FileDownloadError::NoLoadableSegments);
        }

        log::info!("Found {} loadable sections:", num_sections);

        for section in &extracted_data {
            let source = if section.section_names.is_empty() {
                "Unknown".to_string()
            } else if section.section_names.len() == 1 {
                section.section_names[0].to_owned()
            } else {
                "Multiple sections".to_owned()
            };

            log::info!(
                "    {} at {:08X?} ({} byte{})",
                source,
                section.address,
                section.data.len(),
                if section.data.len() == 1 { "" } else { "s" }
            );
        }

        for data in extracted_data {
            self.add_data(data.address, data.data)?;
        }

        Ok(())
    }

    /// Writes all the stored data chunks to flash.
    ///
    /// Requires a session with an attached target that has a known flash algorithm.
    ///
    /// If `do_chip_erase` is `true` the entire flash will be erased.
    pub fn commit(
        &self,
        session: &mut Session,
        options: DownloadOptions<'_>,
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
                1 => algorithms[0],
                _ => *algorithms
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

            let flash_algorithm =
                FlashAlgorithm::assemble_from_raw(raw_flash_algorithm, ram, session.target())?;

            if options.dry_run {
                log::info!("Skipping programming, dry run!");
                if let Some(progress) = options.progress {
                    progress.failed_erasing();
                }
                continue;
            }

            // Program the data.
            let mut flasher = Flasher::new(session, flash_algorithm, region.clone());
            flasher.program(
                builder,
                options.do_chip_erase,
                options.keep_unwritten_bytes,
                true,
                options.progress.unwrap_or(&FlashProgress::new(|_| {})),
            )?
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
