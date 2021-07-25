use ihex::Record;
use probe_rs_target::{
    MemoryRange, MemoryRegion, NvmRegion, RawFlashAlgorithm, TargetDescriptionSource,
};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;

use super::builder::FlashBuilder;
use super::{
    extract_from_elf, BinOptions, DownloadOptions, FileDownloadError, FlashError, FlashProgress,
    Flasher,
};
use crate::memory::MemoryInterface;
use crate::session::Session;
use crate::Target;

/// `FlashLoader` is a struct which manages the flashing of any chunks of data onto any sections of flash.
///
/// Use [add_data()](FlashLoader::add_data) to add a chunk of data.
/// Once you are done adding all your data, use `commit()` to flash the data.
/// The flash loader will make sure to select the appropriate flash region for the right data chunks.
/// Region crossing data chunks are allowed as long as the regions are contiguous.
pub struct FlashLoader {
    memory_map: Vec<MemoryRegion>,
    builder: FlashBuilder,

    /// Source of the flash description,
    /// used for diagnostics.
    source: TargetDescriptionSource,
}

impl FlashLoader {
    /// Create a new flash loader.
    pub fn new(memory_map: Vec<MemoryRegion>, source: TargetDescriptionSource) -> Self {
        Self {
            memory_map,
            builder: FlashBuilder::new(),
            source,
        }
    }

    /// Check the given address range is completely covered by the memory map,
    /// possibly by multiple memory regions.
    fn check_data_in_memory_map(&mut self, range: Range<u32>) -> Result<(), FlashError> {
        let mut address = range.start;
        while address < range.end {
            match Self::get_region_for_address(&self.memory_map, address) {
                Some(MemoryRegion::Nvm(region)) => address = region.range.end,
                Some(MemoryRegion::Ram(region)) => address = region.range.end,
                _ => {
                    return Err(FlashError::NoSuitableNvm {
                        start: range.start,
                        end: range.end,
                        description_source: self.source.clone(),
                    })
                }
            }
        }
        Ok(())
    }

    /// Stages a chunk of data to be programmed.
    ///
    /// The chunk can cross flash boundaries as long as one flash region connects to another flash region.
    pub fn add_data(&mut self, address: u32, data: &[u8]) -> Result<(), FlashError> {
        log::trace!(
            "Adding data at address {:#010x} with size {} bytes",
            address,
            data.len()
        );

        self.check_data_in_memory_map(address..address + data.len() as u32)?;
        self.builder.add_data(address, data)
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

        let num_sections = extract_from_elf(&mut extracted_data, &elf_buffer)?;

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
        log::debug!("committing FlashLoader!");

        log::debug!("Contents of builder:");
        for (&address, data) in &self.builder.data {
            log::debug!(
                "    data: {:08x}-{:08x} ({} bytes)",
                address,
                address + data.len() as u32,
                data.len()
            );
        }

        log::debug!("Flash algorithms:");
        for algorithm in &session.target().flash_algorithms {
            let Range { start, end } = algorithm.flash_properties.address_range;

            log::debug!(
                "    algo {}: {:08x}-{:08x} ({} bytes)",
                algorithm.name,
                start,
                end,
                end - start
            );
        }

        // Iterate over all memory regions, and program their data.

        if self.memory_map != session.target().memory_map {
            log::warn!("Memory map of flash loader does not match memory map of target!");
        }

        let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();

        // Commit NVM first

        // Iterate all NvmRegions and group them by flash algorithm.
        // This avoids loading the same algorithm twice if it's used for two regions.
        //
        // This also ensures correct operation when chip erase is used. We assume doing a chip erase
        // using a given algorithm erases all regions controlled by it. Therefore, we must do
        // chip erase once per algorithm, not once per region. Otherwise subsequent chip erases will
        // erase previous regions' flashed contents.
        log::debug!("Regions:");
        for region in &self.memory_map {
            if let MemoryRegion::Nvm(region) = region {
                log::debug!(
                    "    region: {:08x}-{:08x} ({} bytes)",
                    region.range.start,
                    region.range.end,
                    region.range.end - region.range.start
                );

                // If we have no data in this region, ignore it.
                // This avoids uselessly initializing and deinitializing its flash algorithm.
                if !self.builder.has_data_in_range(&region.range) {
                    log::debug!("     -- empty, ignoring!");
                    continue;
                }

                let algo = Self::get_flash_algorithm_for_region(region, session.target())?;

                let entry = algos
                    .entry((
                        algo.name.clone(),
                        region
                            .cores
                            .first()
                            .ok_or(FlashError::NoNvmCoreAccess(region.clone()))?
                            .clone(),
                    ))
                    .or_default();
                entry.push(region.clone());

                log::debug!("     -- using algorithm: {}", algo.name);
            }
        }

        if options.dry_run {
            log::info!("Skipping programming, dry run!");

            if let Some(progress) = options.progress {
                progress.failed_filling();
                progress.failed_erasing();
                progress.failed_programming();
            }

            return Ok(());
        }

        // Iterate all flash algorithms we need to use.
        for ((algo_name, core_name), regions) in algos {
            log::debug!("Flashing ranges for algo: {}", algo_name);

            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(&algo_name);
            let algo = algo.unwrap().clone();

            let core = session
                .target()
                .cores
                .iter()
                .position(|c| c.name == core_name)
                .unwrap();
            let mut flasher = Flasher::new(session, core, &algo)?;

            let mut do_chip_erase = options.do_chip_erase;

            // If the flash algo doesn't support erase all, disable chip erase.
            if do_chip_erase && !flasher.is_chip_erase_supported() {
                do_chip_erase = false;
                log::warn!("Chip erase was the selected method to erase the sectors but this chip does not support chip erases (yet).");
                log::warn!("A manual sector erase will be performed.");
            }

            if do_chip_erase {
                log::debug!("    Doing chip erase...");
                flasher.run_erase(|active| active.erase_all())?;
            }

            for region in regions {
                log::debug!(
                    "    programming region: {:08x}-{:08x} ({} bytes)",
                    region.range.start,
                    region.range.end,
                    region.range.end - region.range.start
                );

                // Program the data.
                flasher.program(
                    &region,
                    &self.builder,
                    options.keep_unwritten_bytes,
                    true,
                    options.skip_erase || do_chip_erase,
                    options.progress.unwrap_or(&FlashProgress::new(|_| {})),
                )?;
            }
        }

        log::debug!("committing RAM!");

        // Commit RAM last, because NVM flashing overwrites RAM
        for region in &self.memory_map {
            if let MemoryRegion::Ram(region) = region {
                log::debug!(
                    "    region: {:08x}-{:08x} ({} bytes)",
                    region.range.start,
                    region.range.end,
                    region.range.end - region.range.start
                );

                let region_core_index = session
                    .target()
                    .core_index_by_name(
                        region
                            .cores
                            .first()
                            .ok_or(FlashError::NoRamCoreAccess(region.clone()))?,
                    )
                    .unwrap();
                // Attach to memory and core.
                let mut core = session.core(region_core_index).map_err(FlashError::Core)?;

                let mut some = false;
                for (address, data) in self.builder.data_in_range(&region.range) {
                    some = true;
                    log::debug!(
                        "     -- writing: {:08x}-{:08x} ({} bytes)",
                        address,
                        address + data.len() as u32,
                        data.len()
                    );
                    // Write data to memory.
                    core.write_8(address, data).map_err(FlashError::Core)?;
                }

                if !some {
                    log::debug!("     -- empty.")
                }
            }
        }

        if options.verify {
            log::debug!("Verifying!");
            for (&address, data) in &self.builder.data {
                log::debug!(
                    "    data: {:08x}-{:08x} ({} bytes)",
                    address,
                    address + data.len() as u32,
                    data.len()
                );

                let associated_region = session
                    .target()
                    .get_memory_region_by_address(address)
                    .unwrap();
                let core_name = match associated_region {
                    MemoryRegion::Ram(r) => &r.cores,
                    MemoryRegion::Generic(r) => &r.cores,
                    MemoryRegion::Nvm(r) => &r.cores,
                }
                .first()
                .unwrap();
                let core_index = session.target().core_index_by_name(core_name).unwrap();
                let mut core = session.core(core_index).map_err(FlashError::Core)?;

                let mut written_data = vec![0; data.len()];
                core.read(address, &mut written_data)
                    .map_err(FlashError::Core)?;

                if data != &written_data {
                    return Err(FlashError::Verify);
                }
            }
        }

        Ok(())
    }

    /// Try to find a flash algorithm for the given NvmRegion.
    /// Errors when:
    /// - there's no algo for the region.
    /// - there's multiple default algos for the region.
    /// - there's multiple fitting algos but no default.
    pub(crate) fn get_flash_algorithm_for_region<'a>(
        region: &NvmRegion,
        target: &'a Target,
    ) -> Result<&'a RawFlashAlgorithm, FlashError> {
        let algorithms = target
            .flash_algorithms
            .iter()
            // filter for algorithims that contiain adress range
            .filter(|&fa| {
                fa.flash_properties
                    .address_range
                    .contains_range(&region.range)
            })
            .collect::<Vec<_>>();

        match algorithms.len() {
            0 => Err(FlashError::NoFlashLoaderAlgorithmAttached {
                name: target.name.clone(),
            }),
            1 => Ok(algorithms[0]),
            _ => {
                // filter for defaults
                let defaults = algorithms
                    .iter()
                    .filter(|&fa| fa.default)
                    .collect::<Vec<_>>();

                match defaults.len() {
                    0 => Err(FlashError::MultipleFlashLoaderAlgorithmsNoDefault {
                        region: region.clone(),
                    }),
                    1 => Ok(defaults[0]),
                    _ => Err(FlashError::MultipleDefaultFlashLoaderAlgorithms {
                        region: region.clone(),
                    }),
                }
            }
        }
    }
}
