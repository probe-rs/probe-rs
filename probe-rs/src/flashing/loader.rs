use espflash::flasher::{FlashData, FlashSettings};
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

use super::builder::FlashBuilder;
use super::{
    extract_from_elf, BinOptions, DownloadOptions, FileDownloadError, FlashError, Flasher,
    IdfOptions,
};
use crate::config::DebugSequence;
use crate::flashing::Format;
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
        match format {
            Format::Bin(options) => self.load_bin_data(file, options),
            Format::Elf => self.load_elf_data(file),
            Format::Hex => self.load_hex_data(file),
            Format::Idf(options) => self.load_idf_data(session, file, options),
            Format::Uf2 => self.load_uf2_data(file),
        }
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

    /// Loads an esp-idf application into the loader by converting the main application to the esp-idf bootloader format,
    /// appending it to the loader along with the bootloader and partition table.
    ///
    /// This does not create any flash loader instructions yet.
    pub fn load_idf_data<T: Read>(
        &mut self,
        session: &mut Session,
        file: &mut T,
        options: IdfOptions,
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

        let flash_size_result = session.halted_access(|sess| {
            // Figure out flash size from the memory map. We need a different bootloader for each size.
            Ok(match sess.target().debug_sequence.clone() {
                DebugSequence::Riscv(sequence) => {
                    sequence.detect_flash_size(&mut sess.get_riscv_interface()?)
                }
                DebugSequence::Xtensa(sequence) => {
                    sequence.detect_flash_size(&mut sess.get_xtensa_interface()?)
                }
                DebugSequence::Arm(_) => panic!("There are no ARM ESP targets."),
            })
        })?;

        let flash_size = match flash_size_result.map_err(FileDownloadError::FlashSizeDetection)? {
            Some(0x40000) => Some(espflash::flasher::FlashSize::_256Kb),
            Some(0x80000) => Some(espflash::flasher::FlashSize::_512Kb),
            Some(0x100000) => Some(espflash::flasher::FlashSize::_1Mb),
            Some(0x200000) => Some(espflash::flasher::FlashSize::_2Mb),
            Some(0x400000) => Some(espflash::flasher::FlashSize::_4Mb),
            Some(0x800000) => Some(espflash::flasher::FlashSize::_8Mb),
            Some(0x1000000) => Some(espflash::flasher::FlashSize::_16Mb),
            Some(0x2000000) => Some(espflash::flasher::FlashSize::_32Mb),
            Some(0x4000000) => Some(espflash::flasher::FlashSize::_64Mb),
            Some(0x8000000) => Some(espflash::flasher::FlashSize::_128Mb),
            Some(0x10000000) => Some(espflash::flasher::FlashSize::_256Mb),
            _ => None,
        };

        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        let firmware = espflash::elf::ElfFirmwareImage::try_from(&buf[..])?;

        let flash_data = FlashData::new(
            options.bootloader.as_deref(),
            options.partition_table.as_deref(),
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
            self.add_data(data.addr.into(), &data.data)?;
        }

        Ok(())
    }

    /// Reads the HEX data segments and adds them as loadable data blocks to the loader.
    /// This does not create any flash loader instructions yet.
    pub fn load_hex_data<T: Read>(&mut self, file: &mut T) -> Result<(), FileDownloadError> {
        let mut base_address = 0;

        let mut data = String::new();
        file.read_to_string(&mut data)?;

        for record in ihex::Reader::new(&data) {
            match record? {
                Record::Data { offset, value } => {
                    let offset = base_address + offset as u64;
                    self.add_data(offset, &value)?;
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

    /// Prepares the data sections that have to be loaded into flash from an ELF file.
    /// This will validate the ELF file and transform all its data into sections but no flash loader commands yet.
    pub fn load_elf_data<T: Read>(&mut self, file: &mut T) -> Result<(), FileDownloadError> {
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
            self.add_data(data.address.into(), data.data)?;
        }

        Ok(())
    }

    /// Prepares the data sections that have to be loaded into flash from an UF2 file.
    /// This will validate the UF2 file and transform all its data into sections but no flash loader commands yet.
    pub fn load_uf2_data<T: Read>(&mut self, file: &mut T) -> Result<(), FileDownloadError> {
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
            self.add_data(*target_address, &converted)?;

            Ok(())
        } else {
            tracing::warn!("No loadable segments were found in the UF2 file.");
            Err(FileDownloadError::NoLoadableSegments)
        }
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

        let mut algos: HashMap<(String, String), Vec<NvmRegion>> = HashMap::new();

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

            let algo = Self::get_flash_algorithm_for_region(region, session.target())?;

            let entry = algos
                .entry((
                    algo.name.clone(),
                    region
                        .cores
                        .first()
                        .ok_or_else(|| FlashError::NoNvmCoreAccess(region.clone()))?
                        .clone(),
                ))
                .or_default();
            entry.push(region.clone());

            tracing::debug!("     -- using algorithm: {}", algo.name);
        }

        if options.dry_run {
            tracing::info!("Skipping programming, dry run!");

            if let Some(progress) = options.progress {
                progress.failed_filling();
                progress.failed_erasing();
                progress.failed_programming();
            }

            return Ok(());
        }

        // Iterate all flash algorithms we need to use.
        for ((algo_name, core_name), regions) in algos {
            tracing::debug!("Flashing ranges for algo: {}", algo_name);

            // This can't fail, algo_name comes from the target.
            let algo = session.target().flash_algorithm_by_name(&algo_name);
            let algo = algo.unwrap().clone();

            let core = session
                .target()
                .cores
                .iter()
                .position(|c| c.name == core_name)
                .unwrap();
            let mut flasher = Flasher::new(session, core, &algo, options.progress.clone())?;

            let mut do_chip_erase = options.do_chip_erase;

            // If the flash algo doesn't support erase all, disable chip erase.
            if do_chip_erase && !flasher.is_chip_erase_supported() {
                do_chip_erase = false;
                tracing::warn!("Chip erase was the selected method to erase the sectors but this chip does not support chip erases (yet).");
                tracing::warn!("A manual sector erase will be performed.");
            }

            if do_chip_erase {
                tracing::debug!("    Doing chip erase...");
                flasher.run_erase_all()?;
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
                    options.skip_erase || do_chip_erase,
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
