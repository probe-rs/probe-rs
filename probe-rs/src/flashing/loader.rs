use probe_rs_target::{
    InstructionSet, MemoryRange, MemoryRegion, NvmRegion, RawFlashAlgorithm,
    TargetDescriptionSource,
};
use std::collections::HashMap;
use std::io::{Read, Seek};
use std::ops::Range;

use super::builder::FlashBuilder;
use super::{DownloadOptions, FileDownloadError, FlashError, Flasher};
use crate::flashing::image::Format;
use crate::flashing::{FlashLayout, FlashProgress};
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
    fn check_data_in_memory_map(&mut self, range: Range<u64>) -> Result<(), FlashError> {
        let mut address = range.start;
        while address < range.end {
            match Self::get_region_for_address(&self.memory_map, address) {
                Some(MemoryRegion::Nvm(region)) => address = region.range.end,
                Some(MemoryRegion::Ram(region)) => address = region.range.end,
                _ => {
                    return Err(FlashError::NoSuitableNvm {
                        range,
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
    pub fn add_data(&mut self, address: u64, data: &[u8]) -> Result<(), FlashError> {
        tracing::trace!(
            "Adding data at address {:#010x} with size {} bytes",
            address,
            data.len()
        );

        self.check_data_in_memory_map(address..address + data.len() as u64)?;
        self.builder.add_data(address, data)
    }

    pub(super) fn get_region_for_address(
        memory_map: &[MemoryRegion],
        address: u64,
    ) -> Option<&MemoryRegion> {
        memory_map.iter().find(|region| region.contains(address))
    }

    /// Reads the image according to the file format and adds it to the loader.
    pub fn load_image<T: Read + Seek>(
        &mut self,
        session: &mut Session,
        file: &mut T,
        loader: Format,
        image_instruction_set: Option<InstructionSet>,
    ) -> Result<(), FileDownloadError> {
        if let Some(instr_set) = image_instruction_set {
            let mut target_archs = Vec::with_capacity(session.list_cores().len());

            // Get a unique list of core architectures
            for (core, _) in session.list_cores() {
                if let Ok(set) = session.core(core).unwrap().instruction_set() {
                    if !target_archs.contains(&set) {
                        target_archs.push(set);
                    }
                }
            }

            // Is the image compatible with any of the cores?
            if !target_archs
                .iter()
                .any(|target| target.is_compatible(instr_set))
            {
                return Err(FileDownloadError::IncompatibleImage {
                    target: target_archs,
                    image: instr_set,
                });
            }
        }

        loader.load(self, session, file)
    }

    /// Writes all the stored data chunks to flash.
    ///
    /// Requires a session with an attached target that has a known flash algorithm.
    pub fn commit(
        &self,
        session: &mut Session,
        options: DownloadOptions,
    ) -> Result<(), FlashError> {
        tracing::debug!("Committing FlashLoader!");

        tracing::debug!("Contents of builder:");
        for (&address, data) in &self.builder.data {
            tracing::debug!(
                "    data: {:#010X}..{:#010X} ({} bytes)",
                address,
                address + data.len() as u64,
                data.len()
            );
        }

        tracing::debug!("Flash algorithms:");
        for algorithm in &session.target().flash_algorithms {
            let Range { start, end } = algorithm.flash_properties.address_range;

            tracing::debug!(
                "    algo {}: {:#010X}..{:#010X} ({} bytes)",
                algorithm.name,
                start,
                end,
                end - start
            );
        }

        // Iterate over all memory regions, and program their data.

        if self.memory_map != session.target().memory_map {
            tracing::warn!("Memory map of flash loader does not match memory map of target!");
        }

        let mut algos: HashMap<(String, usize), Vec<NvmRegion>> = HashMap::new();

        let progress = options.progress.unwrap_or(FlashProgress::new(|_| {}));

        // Commit NVM first

        // Iterate all NvmRegions and group them by flash algorithm.
        // This avoids loading the same algorithm twice if it's used for two regions.
        //
        // This also ensures correct operation when chip erase is used. We assume doing a chip erase
        // using a given algorithm erases all regions controlled by it. Therefore, we must do
        // chip erase once per algorithm, not once per region. Otherwise subsequent chip erases will
        // erase previous regions' flashed contents.
        tracing::debug!("Regions:");
        for region in self
            .memory_map
            .iter()
            .filter_map(MemoryRegion::as_nvm_region)
        {
            if region.is_alias {
                tracing::debug!("Skipping alias memory region {:#010X?}", region.range);
                continue;
            }
            tracing::debug!(
                "    region: {:#010X?} ({} bytes)",
                region.range,
                region.range.end - region.range.start
            );

            // If we have no data in this region, ignore it.
            // This avoids uselessly initializing and deinitializing its flash algorithm.
            if !self.builder.has_data_in_range(&region.range) {
                tracing::debug!("     -- empty, ignoring!");
                continue;
            }

            let target = session.target();
            let algo = Self::get_flash_algorithm_for_region(region, target)?;
            let core_name = region
                .cores
                .first()
                .ok_or_else(|| FlashError::NoNvmCoreAccess(region.clone()))?
                .clone();

            let core = target
                .cores
                .iter()
                .position(|c| c.name == core_name)
                .unwrap();

            let entry = algos.entry((algo.name.clone(), core)).or_default();
            entry.push(region.clone());

            tracing::debug!("     -- using algorithm: {}", algo.name);
        }

        if options.dry_run {
            tracing::info!("Skipping programming, dry run!");

            progress.failed_filling();
            progress.failed_erasing();
            progress.failed_programming();

            return Ok(());
        }

        let mut do_chip_erase = options.do_chip_erase;
        let mut did_chip_erase = false;

        // No longer needs to be mutable.
        let algos = algos;

        let mut phases = vec![];

        // Iterate all flash algorithms to initialize a few things.
        for ((algo_name, core), regions) in algos.iter() {
            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(algo_name);
            let algo = algo.unwrap().clone();

            let flasher = Flasher::new(session, *core, &algo, progress.clone())?;
            // If the first flash algo doesn't support erase all, disable chip erase.
            // TODO: we could sort by support but it's unlikely to make a difference.
            if do_chip_erase && !flasher.is_chip_erase_supported() {
                do_chip_erase = false;
                tracing::warn!("Chip erase was the selected method to erase the sectors but this chip does not support chip erases (yet).");
                tracing::warn!("A manual sector erase will be performed.");
            }

            let mut phase_layout = FlashLayout::default();
            for region in regions {
                let layout =
                    flasher.flash_layout(region, &self.builder, options.keep_unwritten_bytes)?;

                phase_layout.merge_from(layout);
            }
            phases.push(phase_layout);
        }

        progress.initialized(do_chip_erase, options.keep_unwritten_bytes, phases);

        // Iterate all flash algorithms we need to use and do the flashing.
        for ((algo_name, core), regions) in algos {
            tracing::debug!("Flashing ranges for algo: {}", algo_name);

            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(&algo_name);
            let algo = algo.unwrap().clone();

            let mut flasher = Flasher::new(session, core, &algo, progress.clone())?;

            if do_chip_erase {
                tracing::debug!("    Doing chip erase...");
                flasher.run_erase_all()?;
                do_chip_erase = false;
                did_chip_erase = true;
            }

            let mut do_use_double_buffering = flasher.double_buffering_supported();
            if do_use_double_buffering && options.disable_double_buffering {
                tracing::info!("Disabled double-buffering support for loader via passed option, though target supports it.");
                do_use_double_buffering = false;
            }

            for region in regions {
                tracing::debug!(
                    "    programming region: {:#010X?} ({} bytes)",
                    region.range,
                    region.range.end - region.range.start
                );

                // Program the data.
                flasher.program(
                    &region,
                    &self.builder,
                    options.keep_unwritten_bytes,
                    do_use_double_buffering,
                    options.skip_erase || did_chip_erase,
                )?;
            }
        }

        tracing::debug!("committing RAM!");

        // Commit RAM last, because NVM flashing overwrites RAM
        for region in self
            .memory_map
            .iter()
            .filter_map(MemoryRegion::as_ram_region)
        {
            tracing::debug!(
                "    region: {:#010X?} ({} bytes)",
                region.range,
                region.range.end - region.range.start
            );

            let region_core_index = session
                .target()
                .core_index_by_name(
                    region
                        .cores
                        .first()
                        .ok_or_else(|| FlashError::NoRamCoreAccess(region.clone()))?,
                )
                .unwrap();
            // Attach to memory and core.
            let mut core = session.core(region_core_index).map_err(FlashError::Core)?;

            let mut some = false;
            for (address, data) in self.builder.data_in_range(&region.range) {
                some = true;
                tracing::debug!(
                    "     -- writing: {:#010X}..{:#010X} ({} bytes)",
                    address,
                    address + data.len() as u64,
                    data.len()
                );
                // Write data to memory.
                core.write_8(address, data).map_err(FlashError::Core)?;
            }

            if !some {
                tracing::debug!("     -- empty.")
            }
        }

        if options.verify {
            tracing::debug!("Verifying!");
            for (&address, data) in &self.builder.data {
                tracing::debug!(
                    "    data: {:#010X}..{:#010X} ({} bytes)",
                    address,
                    address + data.len() as u64,
                    data.len()
                );

                let associated_region = session
                    .target()
                    .get_memory_region_by_address(address)
                    .unwrap();
                let core_name = associated_region.cores().first().unwrap();
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
                range: region.range.clone(),
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

    /// Return data chunks stored in the `FlashLoader` as pairs of address and bytes.
    pub fn data(&self) -> impl Iterator<Item = (u64, &[u8])> {
        self.builder
            .data
            .iter()
            .map(|(address, data)| (*address, data.as_slice()))
    }
}
