use espflash::flasher::{FlashData, FlashSettings, FlashSize};
use espflash::targets::XtalFrequency;
use ihex::Record;
use probe_rs_target::{
    InstructionSet, MemoryRange, MemoryRegion, NvmRegion, RawFlashAlgorithm,
    TargetDescriptionSource,
};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::str::FromStr;
use std::time::Duration;

use super::builder::FlashBuilder;
use super::{
    extract_from_elf, BinOptions, DownloadOptions, FileDownloadError, FlashError, Flasher,
    IdfOptions,
};
use crate::config::DebugSequence;
use crate::flashing::{FlashLayout, FlashProgress, Format};
use crate::memory::MemoryInterface;
use crate::session::Session;
use crate::Target;

/// Helper trait for object safety.
pub trait ImageReader: Read + Seek {}
impl<T> ImageReader for T where T: Read + Seek {}

/// Load and parse a firmware in a particular format, and add it to the flash loader.
///
/// Based on the image loader, probe-rs may apply certain transformations to the firmware.
pub trait ImageLoader {
    /// Loads the given image.
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError>;
}

impl ImageLoader for Format {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        match self {
            Format::Bin(options) => BinLoader(options.clone()).load(flash_loader, session, file),
            Format::Elf => ElfLoader.load(flash_loader, session, file),
            Format::Hex => HexLoader.load(flash_loader, session, file),
            Format::Idf(options) => IdfLoader(options.clone()).load(flash_loader, session, file),
            Format::Uf2 => Uf2Loader.load(flash_loader, session, file),
        }
    }
}

/// Reads the data from the binary file and adds it to the loader without splitting it into flash instructions yet.
struct BinLoader(BinOptions);

impl ImageLoader for BinLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        // Skip the specified bytes.
        file.seek(SeekFrom::Start(u64::from(self.0.skip)))?;

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;

        flash_loader.add_data(
            // If no base address is specified use the start of the boot memory.
            // TODO: Implement this as soon as we know targets.
            self.0.base_address.unwrap_or_default(),
            &buf,
        )?;

        Ok(())
    }
}

/// Prepares the data sections that have to be loaded into flash from an ELF file.
/// This will validate the ELF file and transform all its data into sections but no flash loader commands yet.
struct ElfLoader;

impl ImageLoader for ElfLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut elf_buffer = Vec::new();
        file.read_to_end(&mut elf_buffer)?;

        let extracted_data = extract_from_elf(&elf_buffer)?;

        if extracted_data.is_empty() {
            tracing::warn!("No loadable segments were found in the ELF file.");
            return Err(FileDownloadError::NoLoadableSegments);
        }

        tracing::info!("Found {} loadable sections:", extracted_data.len());

        for section in &extracted_data {
            let source = match section.section_names.len() {
                0 => "Unknown",
                1 => section.section_names[0].as_str(),
                _ => "Multiple sections",
            };

            tracing::info!(
                "    {} at {:#010X} ({} byte{})",
                source,
                section.address,
                section.data.len(),
                if section.data.len() == 1 { "" } else { "s" }
            );
        }

        for data in extracted_data {
            flash_loader.add_data(data.address.into(), data.data)?;
        }

        Ok(())
    }
}

/// Reads the HEX data segments and adds them as loadable data blocks to the loader.
/// This does not create any flash loader instructions yet.
struct HexLoader;

impl ImageLoader for HexLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut base_address = 0;

        let mut data = String::new();
        file.read_to_string(&mut data)?;

        for record in ihex::Reader::new(&data) {
            match record? {
                Record::Data { offset, value } => {
                    let offset = base_address + offset as u64;
                    flash_loader.add_data(offset, &value)?;
                }
                Record::ExtendedSegmentAddress(address) => {
                    base_address = (address as u64) * 16;
                }
                Record::ExtendedLinearAddress(address) => {
                    base_address = (address as u64) << 16;
                }

                Record::EndOfFile
                | Record::StartSegmentAddress { .. }
                | Record::StartLinearAddress(_) => {}
            }
        }
        Ok(())
    }
}

/// Prepares the data sections that have to be loaded into flash from an UF2 file.
/// This will validate the UF2 file and transform all its data into sections but no flash loader commands yet.
struct Uf2Loader;

impl ImageLoader for Uf2Loader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        _session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let mut uf2_buffer = Vec::new();
        file.read_to_end(&mut uf2_buffer)?;

        let (converted, family_to_target) = uf2_decode::convert_from_uf2(&uf2_buffer).unwrap();
        let target_addresses = family_to_target.values();
        let num_sections = family_to_target.len();

        if let Some(target_address) = target_addresses.min() {
            tracing::info!("Found {} loadable sections:", num_sections);
            if num_sections > 1 {
                tracing::warn!("More than 1 section found in UF2 file.  Using first section.");
            }
            flash_loader.add_data(*target_address, &converted)?;

            Ok(())
        } else {
            tracing::warn!("No loadable segments were found in the UF2 file.");
            Err(FileDownloadError::NoLoadableSegments)
        }
    }
}

/// Loads an ELF file as an esp-idf application into the loader by converting the main application
/// to the esp-idf bootloader format, appending it to the loader along with the bootloader and
/// partition table.
///
/// This does not create any flash loader instructions yet.
struct IdfLoader(IdfOptions);

impl ImageLoader for IdfLoader {
    fn load(
        &self,
        flash_loader: &mut FlashLoader,
        session: &mut Session,
        file: &mut dyn ImageReader,
    ) -> Result<(), FileDownloadError> {
        let target = session.target();
        let target_name = target
            .name
            .split_once('-')
            .map(|(name, _)| name)
            .unwrap_or(target.name.as_str());
        let chip = espflash::targets::Chip::from_str(target_name)
            .map_err(|_| FileDownloadError::IdfUnsupported(target.name.to_string()))?
            .into_target();

        // FIXME: Short-term hack until we can auto-detect the crystal frequency. ESP32 and ESP32-C2
        // have 26MHz and 40MHz options, ESP32-H2 is 32MHz, the rest is 40MHz. We need to specify
        // the frequency because different options require different bootloader images.
        let xtal_frequency = if target_name.eq_ignore_ascii_case("esp32h2") {
            XtalFrequency::_32Mhz
        } else {
            XtalFrequency::_40Mhz
        };

        let flash_size_result = session.halted_access(|session| {
            // Figure out flash size from the memory map. We need a different bootloader for each size.
            match session.target().debug_sequence.clone() {
                DebugSequence::Riscv(sequence) => sequence.detect_flash_size(session),
                DebugSequence::Xtensa(sequence) => sequence.detect_flash_size(session),
                DebugSequence::Arm(_) => panic!("There are no ARM ESP targets."),
            }
        });

        let flash_size = match flash_size_result.map_err(FileDownloadError::FlashSizeDetection)? {
            Some(0x40000) => Some(FlashSize::_256Kb),
            Some(0x80000) => Some(FlashSize::_512Kb),
            Some(0x100000) => Some(FlashSize::_1Mb),
            Some(0x200000) => Some(FlashSize::_2Mb),
            Some(0x400000) => Some(FlashSize::_4Mb),
            Some(0x800000) => Some(FlashSize::_8Mb),
            Some(0x1000000) => Some(FlashSize::_16Mb),
            Some(0x2000000) => Some(FlashSize::_32Mb),
            Some(0x4000000) => Some(FlashSize::_64Mb),
            Some(0x8000000) => Some(FlashSize::_128Mb),
            Some(0x10000000) => Some(FlashSize::_256Mb),
            _ => None,
        };

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let firmware = espflash::elf::ElfFirmwareImage::try_from(&buf[..])?;

        let flash_data = FlashData::new(
            self.0.bootloader.as_deref(),
            self.0.partition_table.as_deref(),
            None,
            None,
            {
                let mut settings = FlashSettings::default();

                settings.size = flash_size;

                settings
            },
            0,
        )?;

        let image = chip.get_flash_image(&firmware, flash_data, None, xtal_frequency)?;

        for data in image.flash_segments() {
            flash_loader.add_data(data.addr.into(), &data.data)?;
        }

        Ok(())
    }
}

/// Status of the successful [`FlashLoader::commit`] operation
#[derive(Debug, Default)]
pub enum FlashCommitInfo {
    /// Relevant for the [`FlashLoader::commit`] caller in order to prepare the chip for booting from RAM
    BootFromRam {
        /// Entry point of the program loaded to RAM
        entry_point: u64,
    },
    /// Core will not be booting from RAM (dry run, boot from flash or just commiting some memory etc.)
    #[default]
    Other,
}

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
    entry_point: Option<u64>,
}

impl FlashLoader {
    /// Create a new flash loader.
    pub fn new(memory_map: Vec<MemoryRegion>, source: TargetDescriptionSource) -> Self {
        Self {
            memory_map,
            builder: FlashBuilder::new(),
            source,
            entry_point: None,
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
        match &mut self.entry_point {
            Some(current_entry_point) => {
                if address < *current_entry_point {
                    *current_entry_point = address;
                }
            }
            None => self.entry_point = Some(address),
        }

        self.builder.add_data(address, data)
    }

    pub(super) fn get_region_for_address(
        memory_map: &[MemoryRegion],
        address: u64,
    ) -> Option<&MemoryRegion> {
        memory_map.iter().find(|region| region.contains(address))
    }

    /// Returns whether an address will be flashed with data
    pub fn has_data_for_address(&self, address: u64) -> bool {
        self.builder.has_data_in_range(&(address..address + 1))
    }

    /// Reads the image according to the file format and adds it to the loader.
    pub fn load_image<T: Read + Seek>(
        &mut self,
        session: &mut Session,
        file: &mut T,
        format: Format,
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

        format.load(self, session, file)
    }

    /// Verifies data on the device.
    pub fn verify(&self, session: &mut Session) -> Result<(), FlashError> {
        let algos = self.prepare_plan(session)?;

        let progress = FlashProgress::new(|_| {});

        // Iterate all flash algorithms we need to use and do the flashing.
        for ((algo_name, core), regions) in algos {
            tracing::debug!("Flashing ranges for algo: {}", algo_name);

            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(&algo_name);
            let algo = algo.unwrap().clone();

            let mut flasher = Flasher::new(session, core, &algo, progress.clone())?;

            for region in regions.iter() {
                let flash_layout = flasher.flash_layout(region, &self.builder, false)?;

                if !flasher.verify(&flash_layout, true)? {
                    return Err(FlashError::Verify);
                }
            }
        }

        self.verify_ram(session)?;

        Ok(())
    }

    /// Writes all the stored data chunks to flash.
    ///
    /// Requires a session with an attached target that has a known flash algorithm.
    pub fn commit(
        &self,
        session: &mut Session,
        mut options: DownloadOptions,
    ) -> Result<FlashCommitInfo, FlashError> {
        tracing::debug!("Committing FlashLoader!");
        let mut commit_info = FlashCommitInfo::default();

        let algos = self.prepare_plan(session)?;

        if options.dry_run {
            tracing::info!("Skipping programming, dry run!");

            if let Some(progress) = options.progress {
                progress.failed_filling();
                progress.failed_erasing();
                progress.failed_programming();
            }

            return Ok(commit_info);
        }

        let progress = options
            .progress
            .clone()
            .unwrap_or_else(FlashProgress::empty);

        self.initialize(&algos, session, &progress, &mut options)?;

        let mut do_chip_erase = options.do_chip_erase;
        let mut did_chip_erase = false;

        if options.preverify && do_chip_erase {
            // This is the simpler solution. We could pre-verify everything up front but it's
            // complex and downloading flash algorithms multiple times may slow the process down.
            tracing::warn!("Pre-verify requested but chip erase is enabled.");
            tracing::warn!(
                "This will erase the entire flash and make pre-verification impossible."
            );
        }

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

            if options.preverify && !did_chip_erase {
                tracing::info!("Pre-verifying!");

                let mut contents_match = true;
                for region in regions.iter() {
                    let flash_layout = flasher.flash_layout(region, &self.builder, false)?;

                    if !flasher.verify(&flash_layout, true)? {
                        contents_match = false;
                        break;
                    }
                }

                if contents_match {
                    tracing::info!("Contents match, skipping flashing.");
                    continue;
                }
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
                    options.verify,
                )?;
            }
        }

        tracing::debug!("Committing RAM!");

        // Commit RAM last, because NVM flashing overwrites RAM
        for region in self
            .memory_map
            .iter()
            .filter_map(MemoryRegion::as_ram_region)
        {
            let ranges_in_region: Vec<_> = self.builder.data_in_range(&region.range).collect();

            if ranges_in_region.is_empty() {
                continue;
            }

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

            // If this is a RAM only flash, the core might still be running. This can be
            // problematic if the instruction RAM is flashed while an application is running, so
            // the core is halted here in any case.
            //
            // Additionally, if entry point is detected in the given RAM region, core should be
            // reset & halted
            if !core.core_halted().map_err(FlashError::Core)? {
                match self.entry_point {
                    Some(entry_point) if region.range.contains(&entry_point) => {
                        commit_info = FlashCommitInfo::BootFromRam { entry_point };
                        tracing::debug!("     -- action: core is not halted and entry point in RAM that is being written, resetting and halting");
                        core.reset_and_halt(Duration::from_millis(500))
                    }
                    _ => {
                        tracing::debug!("     -- action: core is not halted and RAM is being written, halting");
                        core.halt(Duration::from_millis(500))
                    },
                }
                .map_err(FlashError::Core)?;
            }

            for (address, data) in ranges_in_region {
                tracing::debug!(
                    "     -- writing: {:#010X}..{:#010X} ({} bytes)",
                    address,
                    address + data.len() as u64,
                    data.len()
                );
                // Write data to memory.
                core.write(address, data).map_err(FlashError::Core)?;
            }
        }

        if options.verify {
            self.verify_ram(session)?;
        }

        Ok(commit_info)
    }

    fn prepare_plan(
        &self,
        session: &mut Session,
    ) -> Result<HashMap<(String, usize), Vec<NvmRegion>>, FlashError> {
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
            tracing::debug!(
                "    region: {:#010X?} ({} bytes)",
                region.range,
                region.range.end - region.range.start
            );

            // If we have no data in this region, ignore it.
            // This avoids uselessly initializing and deinitializing its flash algorithm.
            // We do not check for alias regions here, as we'll work with them if data explicitly
            // targets them.
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

        Ok(algos)
    }

    fn initialize(
        &self,
        algos: &HashMap<(String, usize), Vec<NvmRegion>>,
        session: &mut Session,
        progress: &FlashProgress,
        options: &mut DownloadOptions,
    ) -> Result<(), FlashError> {
        let mut phases = vec![];

        // Iterate all flash algorithms to initialize a few things.
        for ((algo_name, core), regions) in algos.iter() {
            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(algo_name);
            let algo = algo.unwrap().clone();

            let flasher = Flasher::new(session, *core, &algo, progress.clone())?;
            // If the first flash algo doesn't support erase all, disable chip erase.
            // TODO: we could sort by support but it's unlikely to make a difference.
            if options.do_chip_erase && !flasher.is_chip_erase_supported() {
                options.do_chip_erase = false;
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

        progress.initialized(options.do_chip_erase, options.keep_unwritten_bytes, phases);

        Ok(())
    }

    fn verify_ram(&self, session: &mut Session) -> Result<(), FlashError> {
        tracing::debug!("Verifying RAM!");
        for (&address, data) in &self.builder.data {
            tracing::debug!(
                "    data: {:#010X}..{:#010X} ({} bytes)",
                address,
                address + data.len() as u64,
                data.len()
            );

            let associated_region = session.target().memory_region_by_address(address).unwrap();

            // We verified NVM regions before, in flasher.program().
            if !associated_region.is_ram() {
                continue;
            }

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
            // filter for algorithms that contain address range
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
