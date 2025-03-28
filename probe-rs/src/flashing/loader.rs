use espflash::flasher::{FlashData, FlashSettings, FlashSize};
use espflash::targets::XtalFrequency;
use ihex::Record;
use probe_rs_target::{
    InstructionSet, MemoryRange, MemoryRegion, NvmRegion, RawFlashAlgorithm,
    TargetDescriptionSource,
};
use std::io::{Read, Seek, SeekFrom};
use std::ops::Range;
use std::str::FromStr;
use std::time::Duration;

use super::builder::FlashBuilder;
use super::{
    BinOptions, DownloadOptions, FileDownloadError, FlashError, Flasher, IdfOptions,
    extract_from_elf,
};
use crate::Target;
use crate::config::DebugSequence;
use crate::flashing::progress::ProgressOperation;
use crate::flashing::{FlashLayout, FlashProgress, Format};
use crate::memory::MemoryInterface;
use crate::session::Session;

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
        const VECTOR_TABLE_SECTION_NAME: &str = ".vector_table";
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

            if source == VECTOR_TABLE_SECTION_NAME {
                flash_loader.set_vector_table_addr(section.address as _);
            }

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
            self.0.target_app_partition.clone(),
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

/// Current boot information
#[derive(Clone, Debug, Default)]
pub enum BootInfo {
    /// Loaded executable has a vector table in RAM
    FromRam {
        /// Address of the vector table in memory
        vector_table_addr: u64,
        /// All cores that should be reset and halted before any RAM access
        cores_to_reset: Vec<String>,
    },
    /// Executable is either not loaded yet or will be booted conventionally (from flash etc.)
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
    /// Relevant for manually configured RAM booted executables, available only if given loader supports it
    vector_table_addr: Option<u64>,
}

impl FlashLoader {
    /// Create a new flash loader.
    pub fn new(memory_map: Vec<MemoryRegion>, source: TargetDescriptionSource) -> Self {
        Self {
            memory_map,
            builder: FlashBuilder::new(),
            source,
            vector_table_addr: None,
        }
    }

    fn set_vector_table_addr(&mut self, vector_table_addr: u64) {
        self.vector_table_addr = Some(vector_table_addr);
    }

    /// Retrieve available boot information
    pub fn boot_info(&self) -> BootInfo {
        let Some(vector_table_addr) = self.vector_table_addr else {
            return BootInfo::Other;
        };

        match Self::get_region_for_address(&self.memory_map, vector_table_addr) {
            Some(MemoryRegion::Ram(region)) => BootInfo::FromRam {
                vector_table_addr,
                cores_to_reset: region.cores.clone(),
            },
            _ => BootInfo::Other,
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
                    });
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
                match session.core(core) {
                    Ok(mut core) => {
                        if let Ok(set) = core.instruction_set() {
                            if !target_archs.contains(&set) {
                                target_archs.push(set);
                            }
                        }
                    }
                    Err(crate::Error::CoreDisabled(_)) => continue,
                    Err(error) => return Err(FileDownloadError::Other(error)),
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
    pub fn verify(&self, session: &mut Session, progress: FlashProgress) -> Result<(), FlashError> {
        let mut algos = self.prepare_plan(session, false)?;

        for flasher in algos.iter_mut() {
            let mut program_size = 0;
            for region in flasher.regions.iter_mut() {
                program_size += region
                    .data
                    .encoder(flasher.flash_algorithm.transfer_encoding, true)
                    .program_size();
            }
            progress.add_progress_bar(ProgressOperation::Verify, Some(program_size));
        }

        // Iterate all flash algorithms we need to use and do the flashing.
        for mut flasher in algos {
            tracing::debug!(
                "Verifying ranges for algo: {}",
                flasher.flash_algorithm.name
            );

            if !flasher.verify(session, &progress, true)? {
                return Err(FlashError::Verify);
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
    ) -> Result<(), FlashError> {
        tracing::debug!("Committing FlashLoader!");
        let mut algos = self.prepare_plan(session, options.keep_unwritten_bytes)?;

        if options.dry_run {
            tracing::info!("Skipping programming, dry run!");

            if let Some(progress) = options.progress {
                progress.failed_filling();
                progress.failed_erasing();
                progress.failed_programming();
            }

            return Ok(());
        }

        let progress = options
            .progress
            .clone()
            .unwrap_or_else(FlashProgress::empty);

        self.initialize(&mut algos, session, &progress, &mut options)?;

        let run_flash = if options.preverify {
            let mut mismatch = false;
            for flasher in &mut algos {
                tracing::debug!("Preverifying for algo: {}", flasher.flash_algorithm.name);

                if flasher.preverify(session, &progress, !options.keep_unwritten_bytes)? {
                    continue;
                } else {
                    tracing::debug!("    Mismatch");
                    mismatch = true;
                    break;
                }
            }
            mismatch
        } else {
            true
        };

        if run_flash {
            let mut do_chip_erase = options.do_chip_erase;
            let mut did_chip_erase = false;

            // Iterate all flash algorithms we need to use and do the flashing.
            for mut flasher in algos {
                tracing::debug!("Flashing ranges for algo: {}", flasher.flash_algorithm.name);

                if do_chip_erase {
                    tracing::debug!("    Doing chip erase...");
                    flasher.run_erase_all(session, &progress)?;
                    do_chip_erase = false;
                    did_chip_erase = true;
                }

                let mut do_use_double_buffering = flasher.double_buffering_supported();
                if do_use_double_buffering && options.disable_double_buffering {
                    tracing::info!(
                        "Disabled double-buffering support for loader via passed option, though target supports it."
                    );
                    do_use_double_buffering = false;
                }

                // Program the data.
                flasher.program(
                    session,
                    &progress,
                    options.keep_unwritten_bytes,
                    do_use_double_buffering,
                    options.skip_erase || did_chip_erase,
                    options.verify,
                )?;
            }
        }

        tracing::debug!("Committing RAM!");

        if let BootInfo::FromRam { cores_to_reset, .. } = self.boot_info() {
            // If we are booting from RAM, it is important to reset and halt to guarantee a clear state
            // Normally, flash algorithm loader performs reset and halt - does not happen here.
            tracing::debug!(
                " -- action: vector table in RAM, assuming RAM boot, resetting and halting"
            );
            for (core_to_reset_index, _) in session
                .target()
                .cores
                .clone()
                .iter()
                .enumerate()
                .filter(|(_, c)| cores_to_reset.contains(&c.name))
            {
                session
                    .core(core_to_reset_index)
                    .and_then(|mut core| core.reset_and_halt(Duration::from_millis(500)))
                    .map_err(FlashError::Core)?;
            }
        }

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
            if !core.core_halted().map_err(FlashError::Core)? {
                tracing::debug!(
                    "     -- action: core is not halted and RAM is being written, halting"
                );
                core.halt(Duration::from_millis(500))
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

        Ok(())
    }

    fn prepare_plan(
        &self,
        session: &mut Session,
        restore_unwritten_bytes: bool,
    ) -> Result<Vec<Flasher>, FlashError> {
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

        let mut algos = Vec::<Flasher>::new();

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

            let region = region.clone();

            let Some(core_name) = region.cores.first() else {
                return Err(FlashError::NoNvmCoreAccess(region));
            };

            let target = session.target();
            let core = target.core_index_by_name(core_name).unwrap();
            let algo = Self::get_flash_algorithm_for_region(&region, target)?;

            // We don't usually have more than a handful of regions, linear search should be fine.
            tracing::debug!("     -- using algorithm: {}", algo.name);
            if let Some(entry) = algos
                .iter_mut()
                .find(|entry| entry.flash_algorithm.name == algo.name && entry.core_index == core)
            {
                entry.add_region(region, &self.builder, restore_unwritten_bytes)?;
            } else {
                let mut flasher = Flasher::new(target, core, algo)?;
                flasher.add_region(region, &self.builder, restore_unwritten_bytes)?;
                algos.push(flasher);
            }
        }

        Ok(algos)
    }

    fn initialize(
        &self,
        algos: &mut [Flasher],
        session: &mut Session,
        progress: &FlashProgress,
        options: &mut DownloadOptions,
    ) -> Result<(), FlashError> {
        let mut phases = vec![];

        for flasher in algos.iter() {
            // If the first flash algo doesn't support erase all, disable chip erase.
            // TODO: we could sort by support but it's unlikely to make a difference.
            if options.do_chip_erase && !flasher.is_chip_erase_supported(session) {
                options.do_chip_erase = false;
                tracing::warn!(
                    "Chip erase was the selected method to erase the sectors but this chip does not support chip erases (yet)."
                );
                tracing::warn!("A manual sector erase will be performed.");
            }
        }

        if options.do_chip_erase {
            progress.add_progress_bar(ProgressOperation::Erase, None);
        }

        // Iterate all flash algorithms to initialize a few things.
        for flasher in algos.iter_mut() {
            let mut phase_layout = FlashLayout::default();

            let mut fill_size = 0;
            let mut erase_size = 0;
            let mut program_size = 0;

            for region in flasher.regions.iter_mut() {
                let layout = region.flash_layout();
                phase_layout.merge_from(layout.clone());

                erase_size += layout.sectors().iter().map(|s| s.size()).sum::<u64>();
                fill_size += layout.fills().iter().map(|s| s.size()).sum::<u64>();
                program_size += region
                    .data
                    .encoder(
                        flasher.flash_algorithm.transfer_encoding,
                        !options.keep_unwritten_bytes,
                    )
                    .program_size();
            }

            if options.preverify {
                progress.add_progress_bar(ProgressOperation::Preverify, Some(program_size));
            }
            if options.keep_unwritten_bytes {
                progress.add_progress_bar(ProgressOperation::Fill, Some(fill_size));
            }
            if !options.do_chip_erase {
                progress.add_progress_bar(ProgressOperation::Erase, Some(erase_size));
            }
            progress.add_progress_bar(ProgressOperation::Program, Some(program_size));
            if options.verify {
                progress.add_progress_bar(ProgressOperation::Verify, Some(program_size));
            }

            phases.push(phase_layout);
        }

        progress.initialized(phases);

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
